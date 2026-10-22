use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
use thiserror::Error;
use uuid::Uuid;

const SERVICE: &str = "Minefia-Audio";

#[derive(Debug, Error)]
pub enum AdminCredentialError {
    #[error("Could not access the system Keychain: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("The Admin API key cannot be empty.")]
    Empty,
    #[error("The Admin API key item name cannot be empty.")]
    EmptyLabel,
    #[error("No Admin API key is selected. Add or select one in Settings → API keys.")]
    Missing,
    #[error("The selected Admin API key no longer exists.")]
    UnknownKey,
    #[error("Could not read or write Admin API key settings: {0}")]
    Storage(#[from] std::io::Error),
    #[error("Admin API key settings are invalid: {0}")]
    InvalidRegistry(#[from] serde_json::Error),
    #[error("Could not resolve the application data directory: {0}")]
    AppData(#[from] tauri::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKeySummary {
    pub id: String,
    pub label: String,
    pub masked: String,
    pub is_selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyMetadata {
    id: String,
    label: String,
    last_four: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    selected_id: Option<String>,
    keys: Vec<KeyMetadata>,
}

pub fn list(app: &AppHandle) -> Result<Vec<AdminKeySummary>, AdminCredentialError> {
    let mut registry = load_registry(app)?;
    let original_len = registry.keys.len();
    let mut available = Vec::with_capacity(original_len);
    for key in registry.keys.drain(..) {
        match entry(&key.id)?.get_password() {
            Ok(_) => available.push(key),
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
    }
    registry.keys = available;
    if registry.keys.len() != original_len {
        if !registry
            .selected_id
            .as_ref()
            .is_some_and(|id| registry.keys.iter().any(|key| &key.id == id))
        {
            registry.selected_id = registry.keys.first().map(|key| key.id.clone());
        }
        write_registry(app, &registry)?;
    }
    Ok(registry
        .keys
        .iter()
        .map(|key| AdminKeySummary {
            id: key.id.clone(),
            label: key.label.clone(),
            masked: format!("•••• {}", key.last_four),
            is_selected: registry.selected_id.as_deref() == Some(&key.id),
        })
        .collect())
}

pub fn save(
    app: &AppHandle,
    label: &str,
    api_key: &str,
) -> Result<AdminKeySummary, AdminCredentialError> {
    let api_key = api_key.trim();
    let label = label.trim();
    if api_key.is_empty() {
        return Err(AdminCredentialError::Empty);
    }
    if label.is_empty() {
        return Err(AdminCredentialError::EmptyLabel);
    }

    let mut registry = load_registry(app)?;
    let id = Uuid::new_v4().to_string();
    entry(&id)?.set_password(api_key)?;
    let metadata = KeyMetadata {
        id: id.clone(),
        label: label.to_owned(),
        last_four: api_key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    };
    registry.keys.push(metadata.clone());
    registry.selected_id = Some(id.clone());
    if let Err(error) = write_registry(app, &registry) {
        let _ = entry(&id)?.delete_credential();
        return Err(error);
    }
    Ok(AdminKeySummary {
        id,
        label: metadata.label,
        masked: format!("•••• {}", metadata.last_four),
        is_selected: true,
    })
}

pub fn select(app: &AppHandle, id: &str) -> Result<(), AdminCredentialError> {
    let mut registry = load_registry(app)?;
    if !registry.keys.iter().any(|key| key.id == id) {
        return Err(AdminCredentialError::UnknownKey);
    }
    registry.selected_id = Some(id.to_owned());
    write_registry(app, &registry)
}

pub fn remove(app: &AppHandle, id: &str) -> Result<(), AdminCredentialError> {
    let mut registry = load_registry(app)?;
    let previous_len = registry.keys.len();
    registry.keys.retain(|key| key.id != id);
    if registry.keys.len() == previous_len {
        return Err(AdminCredentialError::UnknownKey);
    }
    if registry.selected_id.as_deref() == Some(id) {
        registry.selected_id = registry.keys.first().map(|key| key.id.clone());
    }
    write_registry(app, &registry)?;
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn load_selected(app: &AppHandle) -> Result<String, AdminCredentialError> {
    let registry = load_registry(app)?;
    let id = registry.selected_id.ok_or(AdminCredentialError::Missing)?;
    entry(&id)?.get_password().map_err(|error| match error {
        keyring::Error::NoEntry => AdminCredentialError::UnknownKey,
        other => other.into(),
    })
}

fn entry(id: &str) -> Result<Entry, AdminCredentialError> {
    Ok(Entry::new(SERVICE, &format!("admin-api-key:{id}"))?)
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, AdminCredentialError> {
    Ok(app.path().app_data_dir()?.join("admin-api-keys.json"))
}

fn load_registry(app: &AppHandle) -> Result<Registry, AdminCredentialError> {
    let path = registry_path(app)?;
    if !path.exists() {
        return Ok(Registry::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_registry(app: &AppHandle, registry: &Registry) -> Result<(), AdminCredentialError> {
    let path = registry_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(registry)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}
