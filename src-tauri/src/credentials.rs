use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
use thiserror::Error;
use uuid::Uuid;

const SERVICE: &str = "Minefia-Audio";
const LEGACY_ACCOUNT: &str = "openai-api-key";

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("Could not access the system Keychain: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("No API key is selected. Add or select one in Settings.")]
    Missing,
    #[error("The API key cannot be empty.")]
    Empty,
    #[error("The API key name cannot be empty.")]
    EmptyLabel,
    #[error("The selected API key no longer exists.")]
    UnknownKey,
    #[error("Could not read or write API key settings: {0}")]
    Storage(#[from] std::io::Error),
    #[error("API key settings are invalid: {0}")]
    InvalidRegistry(#[from] serde_json::Error),
    #[error("Could not resolve the application data directory: {0}")]
    AppData(#[from] tauri::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeySummary {
    pub id: String,
    pub label: String,
    pub masked: String,
    pub is_selected: bool,
    pub openai_key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyMetadata {
    id: String,
    label: String,
    last_four: String,
    #[serde(default)]
    openai_key_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    selected_id: Option<String>,
    keys: Vec<KeyMetadata>,
}

pub fn list(app: &AppHandle) -> Result<Vec<ApiKeySummary>, CredentialError> {
    let mut registry = load_registry_with_migration(app)?;
    let original_len = registry.keys.len();
    let mut available = Vec::with_capacity(original_len);
    for key in registry.keys.drain(..) {
        match key_entry(&key.id)?.get_password() {
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
        .map(|key| ApiKeySummary {
            id: key.id.clone(),
            label: key.label.clone(),
            masked: format!("•••• {}", key.last_four),
            is_selected: registry.selected_id.as_deref() == Some(&key.id),
            openai_key_id: key.openai_key_id.clone(),
        })
        .collect())
}

pub fn save(app: &AppHandle, label: &str, api_key: &str) -> Result<ApiKeySummary, CredentialError> {
    save_with_openai_key_id(app, label, api_key, None)
}

pub fn save_with_openai_key_id(
    app: &AppHandle,
    label: &str,
    api_key: &str,
    openai_key_id: Option<&str>,
) -> Result<ApiKeySummary, CredentialError> {
    let api_key = api_key.trim();
    let label = label.trim();
    if api_key.is_empty() {
        return Err(CredentialError::Empty);
    }
    if label.is_empty() {
        return Err(CredentialError::EmptyLabel);
    }

    let mut registry = load_registry_with_migration(app)?;
    let previous_selection = if let Some(selected) = registry
        .selected_id
        .as_deref()
        .filter(|selected| registry.keys.iter().any(|key| key.id == *selected))
    {
        match key_entry(selected)?.get_password() {
            Ok(_) => Some(selected.to_owned()),
            Err(keyring::Error::NoEntry) => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let id = Uuid::new_v4().to_string();
    key_entry(&id)?.set_password(api_key)?;
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
        openai_key_id: openai_key_id.map(str::to_owned),
    };
    registry.keys.push(metadata.clone());
    registry.selected_id = previous_selection.or_else(|| Some(id.clone()));
    let is_selected = registry.selected_id.as_deref() == Some(&id);
    if let Err(error) = write_registry(app, &registry) {
        let _ = key_entry(&id)?.delete_credential();
        return Err(error);
    }

    Ok(ApiKeySummary {
        id,
        label: metadata.label,
        masked: format!("•••• {}", metadata.last_four),
        is_selected,
        openai_key_id: metadata.openai_key_id,
    })
}

pub fn has_openai_key_id(app: &AppHandle, openai_key_id: &str) -> Result<bool, CredentialError> {
    let registry = load_registry_with_migration(app)?;
    for key in registry
        .keys
        .iter()
        .filter(|key| key.openai_key_id.as_deref() == Some(openai_key_id))
    {
        match key_entry(&key.id)?.get_password() {
            Ok(_) => return Ok(true),
            Err(keyring::Error::NoEntry) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

pub fn select(app: &AppHandle, id: &str) -> Result<(), CredentialError> {
    let mut registry = load_registry_with_migration(app)?;
    if !registry.keys.iter().any(|key| key.id == id) {
        return Err(CredentialError::UnknownKey);
    }
    registry.selected_id = Some(id.to_owned());
    write_registry(app, &registry)
}

pub fn remove(app: &AppHandle, id: &str) -> Result<(), CredentialError> {
    let mut registry = load_registry_with_migration(app)?;
    let previous_len = registry.keys.len();
    registry.keys.retain(|key| key.id != id);
    if registry.keys.len() == previous_len {
        return Err(CredentialError::UnknownKey);
    }
    if registry.selected_id.as_deref() == Some(id) {
        registry.selected_id = registry.keys.first().map(|key| key.id.clone());
    }
    write_registry(app, &registry)?;
    match key_entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn load_selected(app: &AppHandle) -> Result<String, CredentialError> {
    let registry = load_registry_with_migration(app)?;
    let id = registry.selected_id.ok_or(CredentialError::Missing)?;
    key_entry(&id)?.get_password().map_err(|error| match error {
        keyring::Error::NoEntry => CredentialError::Missing,
        other => other.into(),
    })
}

/// Returns the OpenAI-managed key ID for the selected default key, if it is a
/// Service Account key created and registered by Minefia Audio.
pub fn selected_openai_key_id(app: &AppHandle) -> Result<Option<String>, CredentialError> {
    let registry = load_registry_with_migration(app)?;
    let Some(id) = registry.selected_id else {
        return Ok(None);
    };
    let Some(key) = registry.keys.iter().find(|key| key.id == id) else {
        return Ok(None);
    };
    match key_entry(&id)?.get_password() {
        Ok(_) => Ok(key.openai_key_id.clone()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn key_entry(id: &str) -> Result<Entry, CredentialError> {
    Ok(Entry::new(SERVICE, &format!("openai-api-key:{id}"))?)
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, CredentialError> {
    Ok(app.path().app_data_dir()?.join("api-keys.json"))
}

fn load_registry(app: &AppHandle) -> Result<Registry, CredentialError> {
    let path = registry_path(app)?;
    if !path.exists() {
        return Ok(Registry::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_registry(app: &AppHandle, registry: &Registry) -> Result<(), CredentialError> {
    let path = registry_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(registry)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn load_registry_with_migration(app: &AppHandle) -> Result<Registry, CredentialError> {
    let mut registry = load_registry(app)?;
    if !registry.keys.is_empty() {
        return Ok(registry);
    }

    let legacy = Entry::new(SERVICE, LEGACY_ACCOUNT)?;
    let Ok(password) = legacy.get_password() else {
        return Ok(registry);
    };
    let id = Uuid::new_v4().to_string();
    key_entry(&id)?.set_password(&password)?;
    registry.keys.push(KeyMetadata {
        id: id.clone(),
        label: "Default key".into(),
        last_four: password
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        openai_key_id: None,
    });
    registry.selected_id = Some(id);
    write_registry(app, &registry)?;
    let _ = legacy.delete_credential();
    Ok(registry)
}
