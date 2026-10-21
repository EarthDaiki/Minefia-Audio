use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};
use tauri::Manager;
use uuid::Uuid;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedTranscript {
    pub id: String,
    pub name: String,
    pub date: String,
    pub text: String,
    pub kind: String,
    pub path: String,
}

#[derive(Deserialize, Serialize)]
struct TranscriptMetadata {
    #[serde(default)]
    id: String,
    name: String,
}

static SNAPSHOT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn transcript_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    crate::storage::transcript_root(app)
}

fn legacy_transcript_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recordings").join("transcripts"))
        .map_err(|error| error.to_string())
}

async fn session_dirs(root: &PathBuf) -> Result<Vec<PathBuf>, String> {
    let mut sessions = Vec::new();
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
        Err(error) => return Err(error.to_string()),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        if !entry
            .file_type()
            .await
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if Uuid::parse_str(&name).is_ok()
            || tokio::fs::try_exists(entry.path().join("session.json"))
                .await
                .unwrap_or(false)
        {
            sessions.push(entry.path());
            continue;
        }
        let mut day_entries = match tokio::fs::read_dir(entry.path()).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        while let Some(session) = day_entries
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            if session
                .file_type()
                .await
                .map_err(|error| error.to_string())?
                .is_dir()
                && (Uuid::parse_str(&session.file_name().to_string_lossy()).is_ok()
                    || tokio::fs::try_exists(session.path().join("session.json"))
                        .await
                        .unwrap_or(false))
            {
                sessions.push(session.path());
            }
        }
    }
    Ok(sessions)
}

async fn existing_session_dir(app: &tauri::AppHandle, id: &str) -> Result<Option<PathBuf>, String> {
    for root in [transcript_root(app)?, legacy_transcript_root(app)?] {
        for path in session_dirs(&root).await? {
            if path.file_name().is_some_and(|name| name == id) {
                return Ok(Some(path));
            }
            let metadata = tokio::fs::read(path.join("session.json"))
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<TranscriptMetadata>(&bytes).ok());
            if metadata.is_some_and(|metadata| metadata.id == id) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

async fn new_session_dir(root: &PathBuf, id: &str, name: &str) -> Result<PathBuf, String> {
    let date_dir = root.join(crate::storage::utc_date_now());
    tokio::fs::create_dir_all(&date_dir)
        .await
        .map_err(|error| error.to_string())?;
    let base_name = format!(
        "{}_{}",
        crate::storage::utc_time_now(),
        safe_audio_stem(name)
    );
    let mut candidate = date_dir.join(&base_name);
    let mut collision = 2usize;
    while tokio::fs::try_exists(&candidate)
        .await
        .map_err(|error| error.to_string())?
    {
        candidate = date_dir.join(format!("{base_name}_{collision:02}"));
        collision += 1;
    }
    tokio::fs::create_dir_all(&candidate)
        .await
        .map_err(|error| error.to_string())?;
    let metadata = serde_json::to_vec(&TranscriptMetadata {
        id: id.to_owned(),
        name: name.chars().take(180).collect(),
    })
    .map_err(|error| error.to_string())?;
    tokio::fs::write(candidate.join("session.json"), metadata)
        .await
        .map_err(|error| error.to_string())?;
    Ok(candidate)
}

pub async fn append(
    app: &tauri::AppHandle,
    id: &str,
    chunk: usize,
    attempt: usize,
    name: &str,
    text: &str,
) -> Result<(), String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    if text.is_empty() {
        return Ok(());
    }
    let root = transcript_root(app)?;
    let session_id = id.to_string();
    let directory = match existing_session_dir(app, &session_id).await? {
        Some(path) if path.starts_with(legacy_transcript_root(app)?) => {
            let destination = root.join(crate::storage::utc_date_now()).join(&session_id);
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            tokio::fs::rename(path, &destination)
                .await
                .map_err(|error| error.to_string())?;
            destination
        }
        Some(path) => path,
        None => new_session_dir(&root, &session_id, name).await?,
    };
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let metadata_path = directory.join("session.json");
    if !tokio::fs::try_exists(&metadata_path)
        .await
        .map_err(|error| error.to_string())?
    {
        let metadata = serde_json::to_vec(&TranscriptMetadata {
            id: session_id.clone(),
            name: name.chars().take(180).collect(),
        })
        .map_err(|error| error.to_string())?;
        tokio::fs::write(metadata_path, metadata)
            .await
            .map_err(|error| error.to_string())?;
    }
    let path = directory.join(format!("{chunk:08}-{attempt:08}.txt"));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| error.to_string())?;
    use tokio::io::AsyncWriteExt;
    file.write_all(text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    file.flush().await.map_err(|error| error.to_string())
}

/// Replaces streamed provisional text with the canonical completed API response for a chunk.
pub async fn replace_chunk(
    app: &tauri::AppHandle,
    id: &str,
    chunk: usize,
    attempt: usize,
    name: &str,
    text: &str,
) -> Result<(), String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    let root = transcript_root(app)?;
    let session_id = id.to_string();
    let directory = match existing_session_dir(app, &session_id).await? {
        Some(path) => path,
        None => new_session_dir(&root, &session_id, name).await?,
    };
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let metadata_path = directory.join("session.json");
    if !tokio::fs::try_exists(&metadata_path)
        .await
        .map_err(|error| error.to_string())?
    {
        let metadata = serde_json::to_vec(&TranscriptMetadata {
            id: session_id.clone(),
            name: name.chars().take(180).collect(),
        })
        .map_err(|error| error.to_string())?;
        tokio::fs::write(metadata_path, metadata)
            .await
            .map_err(|error| error.to_string())?;
    }
    let output = directory.join(format!("{chunk:08}-{attempt:08}.txt"));
    let temporary = directory.join(format!(".{chunk:08}-{attempt:08}-{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&temporary, text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(temporary, output)
        .await
        .map_err(|error| error.to_string())
}

pub async fn save_summary(
    app: &tauri::AppHandle,
    id: &str,
    name: &str,
    text: &str,
) -> Result<(), String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let root = transcript_root(app)?;
    let session_id = id.to_string();
    let directory = match existing_session_dir(app, &session_id).await? {
        Some(path) if path.starts_with(legacy_transcript_root(app)?) => {
            let destination = root.join(crate::storage::utc_date_now()).join(&session_id);
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            tokio::fs::rename(path, &destination)
                .await
                .map_err(|error| error.to_string())?;
            destination
        }
        Some(path) => path,
        None => new_session_dir(&root, &session_id, name).await?,
    };
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let metadata_path = directory.join("session.json");
    if !tokio::fs::try_exists(&metadata_path)
        .await
        .map_err(|error| error.to_string())?
    {
        let metadata = serde_json::to_vec(&TranscriptMetadata {
            id: session_id.clone(),
            name: name.chars().take(180).collect(),
        })
        .map_err(|error| error.to_string())?;
        tokio::fs::write(metadata_path, metadata)
            .await
            .map_err(|error| error.to_string())?;
    }
    let temporary = directory.join("summary.md.tmp");
    let summary = directory.join("summary.md");
    tokio::fs::write(&temporary, text.trim())
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(temporary, summary)
        .await
        .map_err(|error| error.to_string())
}

/// Stores a translation beside the transcript it was translated from.
pub async fn save_translation(
    app: &tauri::AppHandle,
    session_id: &str,
    source_name: &str,
    language: &str,
    text: &str,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let session_id = Uuid::parse_str(session_id)
        .map_err(|_| "A transcript session is required to save its translation.")?;
    let session = existing_session_dir(app, &session_id.to_string())
        .await?
        .ok_or_else(|| "The source transcript folder could not be found.".to_owned())?;
    let source_stem = safe_audio_stem(source_name.trim_end_matches(".txt"));
    let directory = session.join("translated");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let language = safe_file_stem(language);
    let output = directory.join(format!(
        "{source_stem}__{language}_{}.txt",
        crate::storage::timestamp_millis()
    ));
    tokio::fs::write(output, text.as_bytes())
        .await
        .map_err(|error| error.to_string())
}

/// Stores a translated summary separately from transcript translations.
pub async fn save_summary_translation(
    app: &tauri::AppHandle,
    session_id: &str,
    source_name: &str,
    language: &str,
    text: &str,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let session_id = Uuid::parse_str(session_id)
        .map_err(|_| "A transcript session is required to save its summary translation.")?;
    let session = existing_session_dir(app, &session_id.to_string())
        .await?
        .ok_or_else(|| "The source transcript folder could not be found.".to_owned())?;
    let directory = session.join("summary-translations");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let stem = safe_audio_stem(source_name.trim_end_matches(".txt"));
    let language = safe_file_stem(language);
    let file_name = format!(
        "{stem}__{language}_{}.txt",
        crate::storage::timestamp_millis()
    );
    tokio::fs::write(directory.join(file_name), text.as_bytes())
        .await
        .map_err(|error| error.to_string())
}

/// Writes a readable, up-to-date transcript file alongside the resumable chunk records.
pub async fn snapshot_transcript(
    app: &tauri::AppHandle,
    id: &str,
    name: &str,
) -> Result<PathBuf, String> {
    let _guard = SNAPSHOT_LOCK.lock().await;
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    let session = existing_session_dir(app, &id.to_string())
        .await?
        .ok_or_else(|| "The transcript session has not been saved yet.".to_owned())?;
    let text = read_session_transcript(&session).await?;
    let metadata_path = session.join("session.json");
    let metadata = serde_json::to_vec(&TranscriptMetadata {
        id: id.to_string(),
        name: name.chars().take(180).collect(),
    })
    .map_err(|error| error.to_string())?;
    let operation_id = Uuid::new_v4();
    let metadata_tmp = session.join(format!(".session-{operation_id}.tmp"));
    tokio::fs::write(&metadata_tmp, metadata)
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(metadata_tmp, metadata_path)
        .await
        .map_err(|error| error.to_string())?;
    let file_name = format!("{}.txt", safe_audio_stem(name));
    let output = session.join(file_name);
    let temporary = session.join(format!(".transcript-output-{operation_id}.tmp"));
    tokio::fs::write(&temporary, text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(&temporary, &output)
        .await
        .map_err(|error| error.to_string())?;
    Ok(output)
}

pub async fn replace_final_transcript(
    app: &tauri::AppHandle,
    id: &str,
    name: &str,
    text: &str,
) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    let root = transcript_root(app)?;
    let session = match existing_session_dir(app, &id.to_string()).await? {
        Some(path) => path,
        None => new_session_dir(&root, &id.to_string(), name).await?,
    };
    tokio::fs::create_dir_all(&session)
        .await
        .map_err(|error| error.to_string())?;
    let metadata_path = session.join("session.json");
    let metadata = serde_json::to_vec(&TranscriptMetadata {
        id: id.to_string(),
        name: name.chars().take(180).collect(),
    })
    .map_err(|error| error.to_string())?;
    tokio::fs::write(&metadata_path, metadata)
        .await
        .map_err(|error| error.to_string())?;
    let output = session.join(format!("{}.txt", safe_audio_stem(name)));
    let temporary = session.join(format!(".final-transcript-{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&temporary, text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(&temporary, &output)
        .await
        .map_err(|error| error.to_string())?;
    Ok(output)
}

/// Stores the reviewed version separately so the original transcript stays intact.
pub async fn save_enhanced_transcript(
    app: &tauri::AppHandle,
    id: &str,
    name: &str,
    text: &str,
) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    let root = transcript_root(app)?;
    let session = match existing_session_dir(app, &id.to_string()).await? {
        Some(path) => path,
        None => new_session_dir(&root, &id.to_string(), name).await?,
    };
    tokio::fs::create_dir_all(&session)
        .await
        .map_err(|error| error.to_string())?;
    let metadata_path = session.join("session.json");
    if !tokio::fs::try_exists(&metadata_path)
        .await
        .map_err(|error| error.to_string())?
    {
        let metadata = serde_json::to_vec(&TranscriptMetadata {
            id: id.to_string(),
            name: name.chars().take(180).collect(),
        })
        .map_err(|error| error.to_string())?;
        tokio::fs::write(metadata_path, metadata)
            .await
            .map_err(|error| error.to_string())?;
    }
    let directory = session.join("enhanced");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let output = directory.join("enhanced.txt");
    let temporary = directory.join(format!(".enhanced-{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&temporary, text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(&temporary, &output)
        .await
        .map_err(|error| error.to_string())?;
    Ok(output)
}

async fn read_session_transcript(session: &PathBuf) -> Result<String, String> {
    let mut entries = tokio::fs::read_dir(session)
        .await
        .map_err(|error| error.to_string())?;
    let mut latest = BTreeMap::<usize, (usize, String)>::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = file_name.strip_suffix(".txt") else {
            continue;
        };
        let Some((chunk, attempt)) = stem.split_once('-') else {
            continue;
        };
        let (Ok(chunk), Ok(attempt)) = (chunk.parse::<usize>(), attempt.parse::<usize>()) else {
            continue;
        };
        let text = tokio::fs::read_to_string(entry.path())
            .await
            .map_err(|error| error.to_string())?;
        let replace = latest
            .get(&chunk)
            .is_none_or(|(current, _)| attempt >= *current);
        if replace {
            latest.insert(chunk, (attempt, text));
        }
    }
    if latest.is_empty() {
        let mut entries = tokio::fs::read_dir(session)
            .await
            .map_err(|error| error.to_string())?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name.ends_with(".txt") && !is_chunk_transcript_file(&file_name) {
                return tokio::fs::read_to_string(entry.path())
                    .await
                    .map_err(|error| error.to_string());
            }
        }
    }
    Ok(latest
        .into_values()
        .map(|(_, text)| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

fn is_chunk_transcript_file(file_name: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".txt") else {
        return false;
    };
    let Some((chunk, attempt)) = stem.split_once('-') else {
        return false;
    };
    !chunk.is_empty()
        && !attempt.is_empty()
        && chunk.bytes().all(|byte| byte.is_ascii_digit())
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
}

/// Removes the resumable per-chunk text fragments after the final transcript is saved.
pub async fn cleanup_chunk_transcripts(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let id = Uuid::parse_str(id).map_err(|_| "Invalid transcript session identifier.")?;
    let Some(session) = existing_session_dir(app, &id.to_string()).await? else {
        return Ok(());
    };
    let mut entries = tokio::fs::read_dir(&session)
        .await
        .map_err(|error| error.to_string())?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if is_chunk_transcript_file(&file_name) {
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn safe_file_stem(name: &str) -> String {
    let name_path = PathBuf::from(name);
    let base = name_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Transcript");
    let sanitized: String = base
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if matches!(
                character,
                '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|'
            ) {
                '_'
            } else {
                character
            }
        })
        .take(120)
        .collect();
    let cleaned = sanitized.trim_matches(['.', ' ']);
    if cleaned.is_empty() {
        "Transcript".into()
    } else {
        cleaned.to_owned()
    }
}

fn safe_audio_stem(name: &str) -> String {
    let path = PathBuf::from(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    safe_file_stem(stem)
}

pub async fn list(app: &tauri::AppHandle) -> Result<Vec<SavedTranscript>, String> {
    let mut sessions = session_dirs(&transcript_root(app)?).await?;
    sessions.extend(session_dirs(&legacy_transcript_root(app)?).await?);
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for session in sessions {
        let folder_id = session
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let metadata = tokio::fs::read(session.join("session.json"))
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<TranscriptMetadata>(&bytes).ok());
        let id = metadata
            .as_ref()
            .map(|metadata| metadata.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or(folder_id);
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = metadata
            .map(|metadata| metadata.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("Transcript {}", &id[..8]));
        let text = read_session_transcript(&session).await?;
        let date = session
            .parent()
            .and_then(|path| path.file_name())
            .map(|value| value.to_string_lossy().into_owned())
            .filter(|value| value.len() == 10 && value.as_bytes()[4] == b'-')
            .unwrap_or_else(|| "Older items".into());
        if !text.trim().is_empty() {
            let path = snapshot_transcript(app, &id, &name).await?;
            result.push(SavedTranscript {
                id: id.clone(),
                name: format!("{}.txt", safe_audio_stem(&name)),
                date: date.clone(),
                text,
                kind: "transcript".into(),
                path: path.to_string_lossy().into_owned(),
            });
        }
        let enhanced_path = session.join("enhanced").join("enhanced.txt");
        if let Ok(enhanced_text) = tokio::fs::read_to_string(&enhanced_path).await {
            if !enhanced_text.trim().is_empty() {
                result.push(SavedTranscript {
                    id: format!("enhanced:{id}"),
                    name: format!("{} · Enhanced.txt", safe_audio_stem(&name)),
                    date: date.clone(),
                    text: enhanced_text,
                    kind: "enhanced".into(),
                    path: enhanced_path.to_string_lossy().into_owned(),
                });
            }
        }
        if let Ok(summary) = tokio::fs::read_to_string(session.join("summary.md")).await {
            if !summary.trim().is_empty() {
                result.push(SavedTranscript {
                    id: format!("summary:{id}"),
                    name: format!("{name} · Summary"),
                    date: session
                        .parent()
                        .and_then(|path| path.file_name())
                        .map(|value| value.to_string_lossy().into_owned())
                        .filter(|value| value.len() == 10 && value.as_bytes()[4] == b'-')
                        .unwrap_or_else(|| "Older items".into()),
                    text: summary,
                    kind: "summary".into(),
                    path: session.join("summary.md").to_string_lossy().into_owned(),
                });
            }
        }
        let summary_translations_dir = session.join("summary-translations");
        if let Ok(mut files) = tokio::fs::read_dir(&summary_translations_dir).await {
            while let Some(file) = files
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                if file
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "txt")
                {
                    continue;
                }
                let text = tokio::fs::read_to_string(file.path())
                    .await
                    .map_err(|error| error.to_string())?;
                if text.trim().is_empty() {
                    continue;
                }
                let file_name = file.file_name().to_string_lossy().into_owned();
                let language = file_name
                    .split_once("__")
                    .and_then(|(_, tail)| tail.rsplit_once('_').map(|(language, _)| language))
                    .unwrap_or("Translation");
                result.push(SavedTranscript {
                    id: format!("summary-translation:{id}:{file_name}"),
                    name: format!("Summary · {} → {language}", safe_audio_stem(&name)),
                    date: date.clone(),
                    text,
                    kind: "summary_translation".into(),
                    path: file.path().to_string_lossy().into_owned(),
                });
            }
        }
        let translated_dir = session.join("translated");
        if let Ok(mut files) = tokio::fs::read_dir(&translated_dir).await {
            while let Some(file) = files
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                if file
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "txt")
                {
                    continue;
                }
                let text = tokio::fs::read_to_string(file.path())
                    .await
                    .map_err(|error| error.to_string())?;
                if text.trim().is_empty() {
                    continue;
                }
                let file_name = file.file_name().to_string_lossy().into_owned();
                let file_stem = file_name.trim_end_matches(".txt");
                let language = file_stem
                    .split_once("__")
                    .and_then(|(_, suffix)| suffix.rsplit_once('_').map(|(language, _)| language))
                    .unwrap_or("Translation");
                result.push(SavedTranscript {
                    id: format!("translation:{id}:{file_name}"),
                    name: format!("Transcript · {} → {language}", safe_audio_stem(&name)),
                    date: date.clone(),
                    text,
                    kind: "translation".into(),
                    path: file.path().to_string_lossy().into_owned(),
                });
            }
        }
    }
    let translated_root = transcript_root(app)?.join("translated");
    if let Ok(mut languages) = tokio::fs::read_dir(&translated_root).await {
        while let Some(language_dir) = languages
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            if !language_dir
                .file_type()
                .await
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            let language = language_dir.file_name().to_string_lossy().into_owned();
            let mut dates = tokio::fs::read_dir(language_dir.path())
                .await
                .map_err(|error| error.to_string())?;
            while let Some(date_dir) = dates
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                if !date_dir
                    .file_type()
                    .await
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    continue;
                }
                let date = date_dir.file_name().to_string_lossy().into_owned();
                let mut files = tokio::fs::read_dir(date_dir.path())
                    .await
                    .map_err(|error| error.to_string())?;
                while let Some(file) = files
                    .next_entry()
                    .await
                    .map_err(|error| error.to_string())?
                {
                    if file
                        .path()
                        .extension()
                        .is_none_or(|extension| extension != "txt")
                    {
                        continue;
                    }
                    let text = tokio::fs::read_to_string(file.path())
                        .await
                        .map_err(|error| error.to_string())?;
                    if text.trim().is_empty() {
                        continue;
                    }
                    let id = format!("translation:{}", file.file_name().to_string_lossy());
                    let file_name = file.file_name().to_string_lossy().into_owned();
                    let source_stem = file_name
                        .trim_end_matches(".txt")
                        .split_once("__")
                        .map(|(source, _)| source)
                        .unwrap_or_else(|| file_name.trim_end_matches(".txt"));
                    result.push(SavedTranscript {
                        id,
                        name: format!("Transcript · {source_stem} → {language}"),
                        date: date.clone(),
                        text,
                        kind: "translation".into(),
                        path: file.path().to_string_lossy().into_owned(),
                    });
                }
            }
        }
    }
    result.sort_by(|left, right| right.id.cmp(&left.id));
    Ok(result)
}

pub async fn remove(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    if let Some(rest) = id.strip_prefix("summary-translation:") {
        let (session_id, file_name) = rest
            .split_once(':')
            .ok_or("Invalid summary translation identifier.")?;
        if Uuid::parse_str(session_id).is_err()
            || PathBuf::from(file_name)
                .file_name()
                .is_none_or(|name| name != file_name)
        {
            return Err("Invalid summary translation identifier.".into());
        }
        if let Some(session) = existing_session_dir(app, session_id).await? {
            return match tokio::fs::remove_file(
                session.join("summary-translations").join(file_name),
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            };
        }
        return Ok(());
    }
    if let Some(raw_id) = id.strip_prefix("enhanced:") {
        let session_id = Uuid::parse_str(raw_id)
            .map_err(|_| "Invalid enhanced transcript identifier.")?
            .to_string();
        let Some(session) = existing_session_dir(app, &session_id).await? else {
            return Ok(());
        };
        let canonical_session = tokio::fs::canonicalize(session)
            .await
            .map_err(|error| error.to_string())?;
        let mut allowed = false;
        for root in [transcript_root(app)?, legacy_transcript_root(app)?] {
            if let Ok(root) = tokio::fs::canonicalize(root).await {
                if canonical_session.starts_with(root) {
                    allowed = true;
                    break;
                }
            }
        }
        if !allowed {
            return Err("Transcript is outside Minefia Audio storage.".into());
        }
        return match tokio::fs::remove_file(canonical_session.join("enhanced/enhanced.txt")).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    } else if let Some(file_name) = id.strip_prefix("translation:") {
        let (session_id, file_name) = file_name
            .split_once(':')
            .filter(|(session_id, _)| Uuid::parse_str(session_id).is_ok())
            .map(|(session_id, file_name)| (Some(session_id), file_name))
            .unwrap_or((None, file_name));
        if PathBuf::from(file_name)
            .file_name()
            .is_none_or(|name| name != file_name)
        {
            return Err("Invalid translation identifier.".into());
        }
        if let Some(session_id) = session_id {
            if let Some(session) = existing_session_dir(app, session_id).await? {
                let candidate = session.join("translated").join(file_name);
                return match tokio::fs::remove_file(candidate).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.to_string()),
                };
            }
            return Ok(());
        }
        let root = transcript_root(app)?.join("translated");
        let mut languages = match tokio::fs::read_dir(&root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        while let Some(language_dir) = languages
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let mut dates = match tokio::fs::read_dir(language_dir.path()).await {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            while let Some(date_dir) = dates
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                let candidate = date_dir.path().join(file_name);
                match tokio::fs::remove_file(candidate).await {
                    Ok(()) => return Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        return Ok(());
    }
    let is_summary = id.starts_with("summary:");
    let raw_id = id.strip_prefix("summary:").unwrap_or(id);
    let id = Uuid::parse_str(raw_id).map_err(|_| "Invalid transcript session identifier.")?;
    let Some(directory) = existing_session_dir(app, &id.to_string()).await? else {
        return Ok(());
    };
    let canonical_directory = tokio::fs::canonicalize(directory)
        .await
        .map_err(|error| error.to_string())?;
    let allowed_roots = [transcript_root(app)?, legacy_transcript_root(app)?];
    let mut allowed = false;
    for root in allowed_roots {
        if let Ok(root) = tokio::fs::canonicalize(root).await {
            if canonical_directory.starts_with(root) {
                allowed = true;
                break;
            }
        }
    }
    if !allowed {
        return Err("Transcript is outside Minefia Audio storage.".into());
    }
    if is_summary {
        match tokio::fs::remove_file(canonical_directory.join("summary.md")).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    } else {
        tokio::fs::remove_dir_all(canonical_directory)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_chunk_transcript_file, read_session_transcript, safe_audio_stem, safe_file_stem,
    };

    #[tokio::test]
    async fn assembled_text_uses_latest_attempt_and_matches_final_chunk_spacing() {
        let directory = tempfile::tempdir().expect("temporary session directory");
        tokio::fs::write(
            directory.path().join("00000001-00000000.txt"),
            "stale partial",
        )
        .await
        .expect("write original attempt");
        tokio::fs::write(
            directory.path().join("00000001-00000001.txt"),
            "  canonical first  ",
        )
        .await
        .expect("write retried attempt");
        tokio::fs::write(directory.path().join("00000002-00000000.txt"), " second\n")
            .await
            .expect("write second chunk");
        tokio::fs::write(
            directory.path().join("source.wav.txt"),
            "must not be treated as a chunk",
        )
        .await
        .expect("write materialized output");

        assert_eq!(
            read_session_transcript(&directory.path().to_path_buf())
                .await
                .unwrap(),
            "canonical first\nsecond"
        );
    }

    #[test]
    fn recognizes_only_numbered_chunk_transcript_files_for_cleanup() {
        assert!(is_chunk_transcript_file("00000001-00000000.txt"));
        assert!(is_chunk_transcript_file("00000000-00000002.txt"));
        assert!(!is_chunk_transcript_file("meeting-notes.txt"));
        assert!(!is_chunk_transcript_file("00000001-summary.txt"));
    }

    #[test]
    fn transcript_filename_keeps_source_extension_and_sanitizes_paths() {
        assert_eq!(safe_file_stem("/private/audio/title.mp3"), "title.mp3");
        assert_eq!(safe_file_stem("bad:name.wav"), "bad_name.wav");
        assert_eq!(safe_audio_stem("/private/audio/title.mp3"), "title");
        assert_eq!(safe_audio_stem("meeting.final.wav"), "meeting.final");
    }
}
