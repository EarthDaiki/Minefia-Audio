use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};
use tauri::{AppHandle, Manager};

const MAX_ENTRY_CHARS: usize = 8_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalLogEntry {
    pub task_id: String,
    pub task_name: String,
    pub timestamp: String,
    pub category: String,
    pub message: String,
}

fn safe_component(value: &str, max_chars: usize) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(max_chars)
        .collect::<String>();
    let trimmed = normalized.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn log_path(app: &AppHandle, entry: &TechnicalLogEntry) -> Result<PathBuf, String> {
    let task_name = safe_component(&entry.task_name, 48);
    let task_id = safe_component(&entry.task_id, 64);
    app.path()
        .app_data_dir()
        .map(|directory| {
            directory
                .join("logs")
                .join(format!("{task_name}_{task_id}.log"))
        })
        .map_err(|error| format!("Could not locate app storage: {error}"))
}

pub fn append(app: &AppHandle, entry: TechnicalLogEntry) -> Result<(), String> {
    let path = log_path(app, &entry)?;
    let directory = path
        .parent()
        .ok_or_else(|| "Log directory is unavailable.".to_owned())?;
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;

    let mut entry = entry;
    if entry.category == "Task" && entry.message == "Started." {
        remove_previous_logs_for_task(directory, &entry.task_name, &path)?;
    }
    entry.task_name = entry.task_name.chars().take(80).collect();
    entry.category = entry.category.chars().take(60).collect();
    entry.message = entry.message.chars().take(MAX_ENTRY_CHARS).collect();
    let mut line = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
    line.push(b'\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    file.write_all(&line).map_err(|error| error.to_string())?;
    Ok(())
}

fn remove_previous_logs_for_task(
    directory: &std::path::Path,
    task_name: &str,
    current_path: &std::path::Path,
) -> Result<(), String> {
    let normalized_name = task_name.to_ascii_lowercase();
    let prefixes = if normalized_name.starts_with("standard audio capture ")
        || normalized_name.starts_with("live audio ")
    {
        vec![
            "standard-audio-capture-".to_owned(),
            "live-audio-".to_owned(),
        ]
    } else {
        vec![format!("{}_", safe_component(task_name, 48))]
    };
    let current_name = current_path.file_name();
    for item in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let path = item.path();
        let name = item.file_name();
        let name = name.to_string_lossy();
        if Some(name.as_ref()) != current_name.and_then(|value| value.to_str())
            && prefixes.iter().any(|prefix| name.starts_with(prefix))
            && name.ends_with(".log")
            && item
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
        {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
