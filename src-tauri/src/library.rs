use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::Manager;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFile {
    pub path: String,
    pub name: String,
    pub date: String,
    pub size_bytes: u64,
    pub is_reference: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_prompt: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MediaContextMetadata {
    context_prompt: Option<String>,
}

fn context_metadata_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("media");
    path.with_file_name(format!(".{name}.minefia.json"))
}

pub async fn save_transcription_context(
    app: &tauri::AppHandle,
    path: &Path,
    prompt: String,
) -> Result<(), String> {
    let root = crate::storage::recordings_root(app)?;
    if !path.starts_with(&root) {
        return Err("Transcription context can only be stored for app media.".into());
    }
    let output = context_metadata_path(path);
    let temporary = output.with_extension("json.tmp");
    let data = serde_json::to_vec(&MediaContextMetadata {
        context_prompt: Some(prompt),
    })
    .map_err(|error| error.to_string())?;
    tokio::fs::write(&temporary, data)
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(temporary, output)
        .await
        .map_err(|error| error.to_string())
}

async fn read_transcription_context(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(context_metadata_path(path)).await.ok()?;
    serde_json::from_slice::<MediaContextMetadata>(&bytes)
        .ok()?
        .context_prompt
}

#[derive(Debug, Serialize, Deserialize)]
struct LocalReferences {
    paths: Vec<String>,
}

pub async fn list(app: &tauri::AppHandle) -> Result<Vec<LibraryFile>, String> {
    let root = crate::storage::recordings_root(app)?;
    let mut pending = vec![root.clone()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "transcripts" {
                continue;
            }
            let kind = entry.file_type().await.map_err(|error| error.to_string())?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && is_media_file(&entry.path()) {
                let metadata = entry.metadata().await.map_err(|error| error.to_string())?;
                files.push(LibraryFile {
                    path: entry.path().to_string_lossy().into_owned(),
                    name,
                    date: entry
                        .path()
                        .strip_prefix(&root)
                        .ok()
                        .and_then(|relative| {
                            relative.components().find_map(|component| {
                                let value = component.as_os_str().to_string_lossy();
                                (value.len() == 10 && value.as_bytes().get(4) == Some(&b'-'))
                                    .then(|| value.into_owned())
                            })
                        })
                        .unwrap_or_else(|| "Older items".into()),
                    size_bytes: metadata.len(),
                    is_reference: false,
                    context_prompt: read_transcription_context(&entry.path()).await,
                });
            }
        }
    }
    let references_path = references_path(app)?;
    let references = match tokio::fs::read(&references_path).await {
        Ok(bytes) => serde_json::from_slice::<LocalReferences>(&bytes)
            .unwrap_or(LocalReferences { paths: vec![] }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            LocalReferences { paths: vec![] }
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut valid_paths = Vec::new();
    for value in &references.paths {
        let source = Path::new(value);
        let Ok(canonical) = tokio::fs::canonicalize(source).await else {
            continue;
        };
        if !canonical.is_file() || !is_media_file(&canonical) {
            continue;
        }
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|error| error.to_string())?;
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media")
            .to_owned();
        let date = "Local files".to_owned();
        let path = canonical.to_string_lossy().into_owned();
        valid_paths.push(path.clone());
        if !files.iter().any(|file| file.path == path) {
            files.push(LibraryFile {
                path,
                name,
                date,
                size_bytes: metadata.len(),
                is_reference: true,
                context_prompt: None,
            });
        }
    }
    if valid_paths != references.paths {
        save_references(&references_path, valid_paths).await?;
    }
    files.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(files)
}

pub async fn remove(app: &tauri::AppHandle, requested: &str) -> Result<(), String> {
    let references_path = references_path(app)?;
    let references = read_references(&references_path).await?;
    let requested_path = Path::new(requested);
    let canonical_requested = tokio::fs::canonicalize(requested_path).await.ok();
    if references.paths.iter().any(|path| {
        Path::new(path) == requested_path || canonical_requested.as_deref() == Some(Path::new(path))
    }) {
        let target = canonical_requested
            .clone()
            .ok_or_else(|| "The referenced source file no longer exists.".to_owned())?;
        if !target.is_file() || !is_media_file(&target) {
            return Err("Only referenced audio and video files can be deleted here.".into());
        }
        tokio::fs::remove_file(&target)
            .await
            .map_err(|error| format!("Could not delete the original file: {error}"))?;
        let paths = references
            .paths
            .into_iter()
            .filter(|path| {
                Path::new(path) != requested_path
                    && canonical_requested.as_deref() != Some(Path::new(path))
            })
            .collect();
        return save_references(&references_path, paths).await;
    }
    let root = crate::storage::recordings_root(app)?;
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| error.to_string())?;
    let requested = Path::new(requested);
    let target = tokio::fs::canonicalize(requested)
        .await
        .map_err(|error| error.to_string())?;
    if !target.starts_with(&root) || !target.is_file() || !is_media_file(&target) {
        return Err(
            "Only saved audio and video files in Minefia Audio storage can be deleted here.".into(),
        );
    }
    tokio::fs::remove_file(&target)
        .await
        .map_err(|error| error.to_string())?;
    let _ = tokio::fs::remove_file(context_metadata_path(&target)).await;
    Ok(())
}

pub async fn reveal(app: &tauri::AppHandle, requested: &str) -> Result<(), String> {
    let target = tokio::fs::canonicalize(requested)
        .await
        .map_err(|error| error.to_string())?;
    if !target.is_file() && !target.is_dir() {
        return Err("The selected Library location is unavailable.".into());
    }
    let mut allowed = false;
    for root in [
        crate::storage::recordings_root(app)?,
        crate::storage::transcript_root(app)?,
    ] {
        if let Ok(canonical_root) = tokio::fs::canonicalize(root).await {
            if target.starts_with(canonical_root) {
                allowed = true;
                break;
            }
        }
    }
    if !allowed {
        let refs = read_references(&references_path(app)?).await?;
        for path in refs.paths {
            if tokio::fs::canonicalize(path).await.ok().as_deref() == Some(target.as_path()) {
                allowed = true;
                break;
            }
        }
    }
    if !allowed {
        return Err("The selected path is not a Minefia Library item.".into());
    }
    let directory = if target.is_dir() {
        target.clone()
    } else {
        target
            .parent()
            .ok_or("The selected file has no parent folder.")?
            .to_path_buf()
    };
    #[cfg(target_os = "macos")]
    let result = if target.is_dir() {
        tokio::process::Command::new("open")
            .arg(&directory)
            .status()
            .await
    } else {
        tokio::process::Command::new("open")
            .arg("-R")
            .arg(&target)
            .status()
            .await
    };
    #[cfg(target_os = "windows")]
    let result = if target.is_dir() {
        tokio::process::Command::new("explorer")
            .arg(&directory)
            .status()
            .await
    } else {
        tokio::process::Command::new("explorer")
            .arg(format!("/select,{}", target.to_string_lossy()))
            .status()
            .await
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = tokio::process::Command::new("xdg-open")
        .arg(&directory)
        .status()
        .await;
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    let result: Result<std::process::ExitStatus, std::io::Error> =
        Err(std::io::Error::other("Unsupported platform"));
    let status = result.map_err(|error| format!("Could not open the folder: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("The operating system could not open this folder.".into())
    }
}

pub async fn import_local(_app: &tauri::AppHandle, source: &str) -> Result<LibraryFile, String> {
    let source = tokio::fs::canonicalize(source)
        .await
        .map_err(|error| error.to_string())?;
    if !source.is_file() || !is_media_file(&source) {
        return Err("Choose a supported local audio or video file.".into());
    }
    let inspected = crate::audio::inspect(&source)
        .await
        .map_err(|error| error.to_string())?;
    if inspected.audio_tracks.is_empty() {
        return Err("NO_AUDIO_STREAM: The selected file does not contain an audio track.".into());
    }
    let value = source.to_string_lossy().into_owned();
    if let Ok(root) = crate::storage::recordings_root(_app) {
        if let Ok(root) = tokio::fs::canonicalize(root).await {
            if source.starts_with(root) {
                let metadata = tokio::fs::metadata(&source)
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(LibraryFile {
                    path: value,
                    name: source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("media")
                        .to_owned(),
                    date: source
                        .parent()
                        .and_then(|path| path.file_name())
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Older items".into()),
                    size_bytes: metadata.len(),
                    is_reference: false,
                    context_prompt: read_transcription_context(&source).await,
                });
            }
        }
    }
    let references_path = references_path(_app)?;
    let mut references = read_references(&references_path).await?;
    if !references.paths.contains(&value) {
        references.paths.push(value.clone());
    }
    save_references(&references_path, references.paths).await?;
    let metadata = tokio::fs::metadata(&source)
        .await
        .map_err(|error| error.to_string())?;
    Ok(LibraryFile {
        path: value,
        name: source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media")
            .to_owned(),
        date: "Local files".into(),
        size_bytes: metadata.len(),
        is_reference: true,
        context_prompt: None,
    })
}

fn references_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("local-media-references.json"))
}

async fn read_references(path: &Path) -> Result<LocalReferences, String> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LocalReferences { paths: vec![] })
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn save_references(path: &Path, paths: Vec<String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(
        &temporary,
        serde_json::to_vec(&LocalReferences { paths }).map_err(|error| error.to_string())?,
    )
    .await
    .map_err(|error| error.to_string())?;
    tokio::fs::rename(temporary, path)
        .await
        .map_err(|error| error.to_string())
}

fn is_media_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "3g2"
                | "3gp"
                | "ac3"
                | "aac"
                | "aiff"
                | "amr"
                | "ape"
                | "asf"
                | "avi"
                | "caf"
                | "dts"
                | "flac"
                | "flv"
                | "m4a"
                | "m4v"
                | "mka"
                | "mkv"
                | "m2ts"
                | "mp3"
                | "mp2"
                | "mpa"
                | "mp4"
                | "mpeg"
                | "mpg"
                | "mts"
                | "oga"
                | "ogg"
                | "ogv"
                | "opus"
                | "ra"
                | "tta"
                | "mov"
                | "vob"
                | "wav"
                | "w64"
                | "webm"
                | "wma"
                | "wmv"
        )
    )
}
