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
    #[error("The Admin API response did not include the required project or API key details.")]
    InvalidResponse,
    #[error("Project name and Service Account name are required.")]
    EmptyName,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MinefiaProject {
    pub project_id: String,
    pub project_name: String,
    #[serde(default)]
    pub created_by_app: bool,
    #[serde(default)]
    pub service_accounts: Vec<MinefiaServiceAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinefiaServiceAccount {
    pub service_account_id: String,
    pub service_account_name: String,
    pub api_key_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectRegistry {
    projects: Vec<MinefiaProject>,
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
    pub project: MinefiaProject,
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

/// Returns registered projects and migrates the former single-project record in place.
pub fn list(app: &AppHandle) -> Result<Vec<MinefiaProject>, AppProjectError> {
    let path = state_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    if value.get("projects").is_some() {
        return Ok(serde_json::from_value::<ProjectRegistry>(value)?.projects);
    }

    // Backward compatibility with minefia-project.json from the single-project version.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyProject {
        project_id: Option<String>,
        project_name: Option<String>,
        #[serde(default)]
        created_by_app: bool,
        service_account_id: Option<String>,
        service_account_name: Option<String>,
        api_key_id: Option<String>,
    }
    let legacy: LegacyProject = serde_json::from_value(value)?;
    let projects = match (legacy.project_id, legacy.project_name) {
        (Some(project_id), Some(project_name)) => {
            let service_accounts = match (
                legacy.service_account_id,
                legacy.service_account_name,
                legacy.api_key_id,
            ) {
                (Some(service_account_id), Some(service_account_name), Some(api_key_id)) => {
                    vec![MinefiaServiceAccount {
                        service_account_id,
                        service_account_name,
                        api_key_id,
                    }]
                }
                _ => Vec::new(),
            };
            vec![MinefiaProject {
                project_id,
                project_name,
                created_by_app: legacy.created_by_app,
                service_accounts,
            }]
        }
        _ => Vec::new(),
    };
    write(app, &projects)?;
    Ok(projects)
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

pub async fn create_project_and_key(
    app: &AppHandle,
    admin_key: &str,
    existing_project_id: Option<&str>,
    project_name: &str,
    service_account_name: &str,
) -> Result<CreatedProjectKey, AppProjectError> {
    let project_name = project_name.trim();
    let service_account_name = service_account_name.trim();
    if project_name.is_empty() || service_account_name.is_empty() {
        return Err(AppProjectError::EmptyName);
    }
    let mut projects = list(app)?;
    let client = admin_client()?;

    let project_id = if let Some(id) = existing_project_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        id.to_owned()
    } else {
        let response = admin_request(
            client
                .clone()
                .post("https://api.openai.com/v1/organization/projects")
                .bearer_auth(admin_key)
                .json(&serde_json::json!({ "name": project_name })),
        )
        .await?;
        let created: ProjectResponse = serde_json::from_value(response)?;
        created.id
    };

    let index = if let Some(index) = projects
        .iter()
        .position(|project| project.project_id == project_id)
    {
        index
    } else {
        projects.push(MinefiaProject {
            project_id: project_id.clone(),
            project_name: project_name.to_owned(),
            created_by_app: existing_project_id.is_none(),
            service_accounts: Vec::new(),
        });
        projects.len() - 1
    };
    // Persist the project before creating a Service Account; the remote Project cannot
    // be rolled back, and the user should be able to retry adding an account after errors.
    write(app, &projects)?;

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

    // Persist the remote account identifier even if Keychain subsequently fails, so the
    // app can show that OpenAI created the account and avoid silently losing its identity.
    let account = MinefiaServiceAccount {
        service_account_id: created.id,
        service_account_name: service_account_name.to_owned(),
        api_key_id: api_key.id.clone(),
    };
    projects[index].service_accounts.push(account);
    write(app, &projects)?;

    // Keep the one-time plaintext key out of app storage; credentials::save writes it to Keychain.
    let saved_key = crate::credentials::save_with_openai_key_id(
        app,
        service_account_name,
        &api_key.value,
        Some(&api_key.id),
    )
    .map_err(|error| AppProjectError::Credential(error.to_string()))?;

    Ok(CreatedProjectKey {
        project: projects[index].clone(),
        key: saved_key,
    })
}

pub fn usage_ready(app: &AppHandle) -> Result<bool, AppProjectError> {
    let Some(selected_key_id) = crate::credentials::selected_openai_key_id(app)
        .map_err(|error| AppProjectError::Credential(error.to_string()))?
    else {
        return Ok(false);
    };
    let projects = list(app)?;
    Ok(projects.iter().any(|project| {
        project
            .service_accounts
            .iter()
            .any(|account| account.api_key_id == selected_key_id)
    }) && !usage_project_ids(app)?.is_empty())
}

pub fn usage_project_ids(app: &AppHandle) -> Result<Vec<String>, AppProjectError> {
    let projects = list(app)?;
    let mut ids = Vec::new();
    for project in projects {
        let linked_key_exists =
            project
                .service_accounts
                .iter()
                .try_fold(false, |found, account| {
                    if found {
                        Ok(true)
                    } else {
                        crate::credentials::has_openai_key_id(app, &account.api_key_id)
                            .map_err(|error| AppProjectError::Credential(error.to_string()))
                    }
                })?;
        if linked_key_exists && !ids.contains(&project.project_id) {
            ids.push(project.project_id);
        }
    }
    Ok(ids)
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

fn write(app: &AppHandle, projects: &[MinefiaProject]) -> Result<(), AppProjectError> {
    let path = state_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&ProjectRegistry {
            projects: projects.to_vec(),
        })?,
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}
