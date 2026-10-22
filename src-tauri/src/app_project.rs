use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppProjectError {
    #[error("Could not resolve the application data directory: {0}")]
    AppData(#[from] tauri::Error),
    #[error("Could not read or write the app project record: {0}")]
    Io(#[from] std::io::Error),
    #[error("The app project record is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Could not access the OpenAI Admin API: {0}")]
    Network(#[from] reqwest::Error),
    #[error("OpenAI Admin API returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Could not save the generated Project API key to Keychain: {0}")]
    Credential(String),
    #[error("A service account has already been created for the Minefia Audio project.")]
    ServiceAccountAlreadyExists,
    #[error("Minefia Audio already created its dedicated Project and cannot switch to a different Project.")]
    ProjectAlreadyOwned,
    #[error("The Admin API response did not include the required project or API key details.")]
    InvalidResponse,
    #[error("Project name and Service Account name are required.")]
    EmptyName,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppProject {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    #[serde(default)]
    pub created_by_app: bool,
    pub service_account_id: Option<String>,
    pub service_account_name: Option<String>,
    pub api_key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedProjectKey {
    pub project: AppProject,
    pub key: crate::credentials::ApiKeySummary,
}

#[derive(Deserialize)]
struct ProjectResponse {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct ProjectListResponse {
    data: Vec<ProjectResponse>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct ServiceAccountResponse {
    id: String,
    api_key: Option<ServiceAccountApiKey>,
}

#[derive(Deserialize)]
struct ServiceAccountApiKey {
    id: String,
    value: String,
}

pub fn get(app: &AppHandle) -> Result<AppProject, AppProjectError> {
    let path = state_path(app)?;
    if !path.exists() {
        return Ok(AppProject::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub async fn list_projects(admin_key: &str) -> Result<Vec<ProjectSummary>, AppProjectError> {
    let client = admin_client()?;
    let mut after: Option<String> = None;
    let mut projects = Vec::new();
    loop {
        let mut request = client
            .get("https://api.openai.com/v1/organization/projects")
            .bearer_auth(admin_key)
            .query(&[("limit", "100"), ("include_archived", "false")]);
        if let Some(cursor) = &after {
            request = request.query(&[("after", cursor)]);
        }
        let response = admin_request(request).await?;
        let page: ProjectListResponse = serde_json::from_value(response)?;
        projects.extend(page.data.into_iter().map(|project| ProjectSummary {
            id: project.id,
            name: project.name.unwrap_or_else(|| "Unnamed project".into()),
            status: project.status.unwrap_or_else(|| "active".into()),
        }));
        if !page.has_more {
            break;
        }
        after = page.last_id;
        if after.is_none() {
            break;
        }
    }
    Ok(projects)
}

pub fn select_existing_project(
    app: &AppHandle,
    project_id: &str,
    project_name: &str,
) -> Result<AppProject, AppProjectError> {
    let mut state = get(app)?;
    if state.created_by_app {
        return Err(AppProjectError::ProjectAlreadyOwned);
    }
    if state.service_account_id.is_some() {
        return Err(AppProjectError::ServiceAccountAlreadyExists);
    }
    let project_id = project_id.trim();
    let project_name = project_name.trim();
    if project_id.is_empty() || project_name.is_empty() {
        return Err(AppProjectError::EmptyName);
    }
    state.project_id = Some(project_id.to_owned());
    state.project_name = Some(project_name.to_owned());
    write(app, &state)?;
    Ok(state)
}

pub async fn create_project_and_key(
    app: &AppHandle,
    admin_key: &str,
    project_name: &str,
    service_account_name: &str,
) -> Result<CreatedProjectKey, AppProjectError> {
    let project_name = project_name.trim();
    let service_account_name = service_account_name.trim();
    if project_name.is_empty() || service_account_name.is_empty() {
        return Err(AppProjectError::EmptyName);
    }
    let mut state = get(app)?;
    let client = admin_client()?;

    if state.project_id.is_none() {
        let response = admin_request(
            client
                .clone()
                .post("https://api.openai.com/v1/organization/projects")
                .bearer_auth(admin_key)
                .json(&serde_json::json!({ "name": project_name })),
        )
        .await?;
        let created: ProjectResponse = serde_json::from_value(response)?;
        state.project_id = Some(created.id);
        state.project_name = Some(project_name.to_owned());
        state.created_by_app = true;
        // Persist immediately: OpenAI projects can be archived, but not deleted.
        write(app, &state)?;
    }

    if state.service_account_id.is_some() {
        return Err(AppProjectError::ServiceAccountAlreadyExists);
    }
    let project_id = state
        .project_id
        .as_deref()
        .ok_or(AppProjectError::InvalidResponse)?;
    let url =
        format!("https://api.openai.com/v1/organization/projects/{project_id}/service_accounts");
    let response = admin_request(
        client
            .post(url)
            .bearer_auth(admin_key)
            .json(&serde_json::json!({ "name": service_account_name })),
    )
    .await?;
    let created: ServiceAccountResponse = serde_json::from_value(response)?;
    let api_key = created.api_key.ok_or(AppProjectError::InvalidResponse)?;

    // Keep the one-time plaintext key out of app storage; credentials::save writes it to Keychain.
    let saved_key = crate::credentials::save_with_openai_key_id(
        app,
        service_account_name,
        &api_key.value,
        Some(&api_key.id),
    )
    .map_err(|error| AppProjectError::Credential(error.to_string()))?;
    state.service_account_id = Some(created.id);
    state.service_account_name = Some(service_account_name.to_owned());
    state.api_key_id = Some(api_key.id);
    write(app, &state)?;

    Ok(CreatedProjectKey {
        project: state,
        key: saved_key,
    })
}

pub fn usage_ready(app: &AppHandle) -> Result<bool, AppProjectError> {
    let project = get(app)?;
    let Some(key_id) = project.api_key_id.as_deref() else {
        return Ok(false);
    };
    Ok(project.project_id.is_some()
        && project.service_account_id.is_some()
        && crate::credentials::has_openai_key_id(app, key_id)
            .map_err(|error| AppProjectError::Credential(error.to_string()))?)
}

fn admin_client() -> Result<reqwest::Client, AppProjectError> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

async fn admin_request(request: reqwest::RequestBuilder) -> Result<Value, AppProjectError> {
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| body.chars().take(600).collect());
        return Err(AppProjectError::Api {
            status: status.as_u16(),
            message,
        });
    }
    Ok(serde_json::from_str(&body)?)
}

fn state_path(app: &AppHandle) -> Result<PathBuf, AppProjectError> {
    Ok(app.path().app_data_dir()?.join("minefia-project.json"))
}

fn write(app: &AppHandle, project: &AppProject) -> Result<(), AppProjectError> {
    let path = state_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(project)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}
