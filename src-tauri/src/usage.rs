use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

const COSTS_URL: &str = "https://api.openai.com/v1/organization/costs";
const ADMIN_API_KEYS_URL: &str = "https://api.openai.com/v1/organization/admin_api_keys";
const LOOKBACK_DAYS: u64 = 180;

#[derive(Debug, Error)]
pub enum CostsError {
    #[error("Could not connect to the OpenAI Costs API: {0}")]
    Network(#[from] reqwest::Error),
    #[error("OpenAI Costs API returned an error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("OpenAI Costs API returned invalid data: {0}")]
    InvalidData(String),
}

#[derive(Debug, Error)]
pub enum AdminKeyValidationError {
    #[error("Could not connect to OpenAI while validating the Admin API key: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Admin API key validation returned an error ({status}): {message}")]
    Api { status: u16, message: String },
}

/// Makes a lightweight authenticated request to confirm the credential is an
/// Admin API key. Cost aggregation is deliberately reserved for Usage refresh.
pub async fn validate_admin_key(api_key: &str) -> Result<(), AdminKeyValidationError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client
        .get(ADMIN_API_KEYS_URL)
        .bearer_auth(api_key)
        .query(&[("limit", "1")])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| body.chars().take(600).collect());
        return Err(AdminKeyValidationError::Api {
            status: status.as_u16(),
            message,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCost {
    pub model: String,
    pub amount_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyProjectCost {
    pub date: String,
    pub models: Vec<ProjectCost>,
    pub total_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCosts {
    pub project_ids: Vec<String>,
    pub days: Vec<DailyProjectCost>,
    pub total_usd: f64,
    pub currency: String,
}

pub async fn fetch(api_key: &str, project_ids: &[String]) -> Result<ProjectCosts, CostsError> {
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(45))
        .build()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let end_time = (now / 86_400 + 1) * 86_400;
    let start_time = end_time.saturating_sub(LOOKBACK_DAYS * 86_400);
    let mut page: Option<String> = None;
    let mut by_day: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();

    loop {
        let mut query = vec![
            ("start_time", start_time.to_string()),
            ("end_time", end_time.to_string()),
            ("bucket_width", "1d".to_owned()),
            ("limit", "180".to_owned()),
            ("group_by", "line_item".to_owned()),
        ];
        query.extend(project_ids.iter().map(|id| ("project_ids", id.clone())));
        let mut request = client.get(COSTS_URL).bearer_auth(api_key).query(&query);
        if let Some(cursor) = &page {
            request = request.query(&[("page", cursor)]);
        }
        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| body.chars().take(600).collect());
            return Err(CostsError::Api {
                status: status.as_u16(),
                message,
            });
        }
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| CostsError::InvalidData(error.to_string()))?;
        let buckets = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| CostsError::InvalidData("missing data buckets".into()))?;
        for bucket in buckets {
            let timestamp = bucket
                .get("start_time")
                .and_then(Value::as_u64)
                .ok_or_else(|| CostsError::InvalidData("bucket has no start_time".into()))?;
            let date = crate::storage::date_from_epoch_seconds(timestamp);
            let rows = bucket
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| CostsError::InvalidData("bucket has no results".into()))?;
            for row in rows {
                let amount = row
                    .pointer("/amount/value")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let line_item = row
                    .get("line_item")
                    .and_then(Value::as_str)
                    .unwrap_or("Other usage");
                let model = line_item
                    .split_once(',')
                    .map(|(name, _)| name.trim())
                    .filter(|name| !name.is_empty())
                    .unwrap_or(line_item.trim())
                    .to_owned();
                *by_day
                    .entry(date.clone())
                    .or_default()
                    .entry(model)
                    .or_default() += amount;
            }
        }
        let has_more = value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        page = value
            .get("next_page")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !has_more || page.is_none() {
            break;
        }
    }

    let mut total_usd = 0.0;
    let mut days = by_day
        .into_iter()
        .map(|(date, models)| {
            let models: Vec<ProjectCost> = models
                .into_iter()
                .map(|(model, amount_usd)| ProjectCost { model, amount_usd })
                .collect();
            let daily_total = models.iter().map(|item| item.amount_usd).sum::<f64>();
            total_usd += daily_total;
            DailyProjectCost {
                date,
                models,
                total_usd: daily_total,
            }
        })
        .collect::<Vec<_>>();
    days.sort_by(|left, right| right.date.cmp(&left.date));
    Ok(ProjectCosts {
        project_ids: project_ids.to_vec(),
        days,
        total_usd,
        currency: "usd".into(),
    })
}
