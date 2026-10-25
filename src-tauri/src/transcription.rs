use crate::audio;
use eventsource_stream::{EventStreamError, Eventsource};
use futures_util::{future::join_all, StreamExt};
use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use thiserror::Error;
use tokio::time::sleep;

#[derive(Default)]
pub struct TranscriptionManager {
    active: Mutex<HashMap<String, ActiveTranscription>>,
}

struct ActiveTranscription {
    task_id: String,
    cancel: Arc<AtomicBool>,
    request_active: bool,
}

impl TranscriptionManager {
    pub fn begin(
        &self,
        task_slot: &str,
        task_id: &str,
    ) -> Result<Arc<AtomicBool>, TranscriptionError> {
        if !matches!(task_slot, "file" | "online") || task_id.is_empty() {
            return Err(TranscriptionError::Job(
                "The transcription task identity is invalid.".into(),
            ));
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| TranscriptionError::Job("Transcription state is unavailable.".into()))?;
        if let Some(current) = active.get(task_slot) {
            if current.task_id == task_id {
                if current.request_active {
                    return Err(TranscriptionError::Job(format!(
                        "A {task_slot} request is already active for this task."
                    )));
                }
                let cancel = current.cancel.clone();
                active
                    .get_mut(task_slot)
                    .expect("entry checked above")
                    .request_active = true;
                return Ok(cancel);
            }
            return Err(TranscriptionError::Job(format!(
                "A {task_slot} transcription is already running."
            )));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        active.insert(
            task_slot.into(),
            ActiveTranscription {
                task_id: task_id.into(),
                cancel: cancel.clone(),
                request_active: true,
            },
        );
        Ok(cancel)
    }

    pub fn cancel(&self, task_slot: &str, task_id: &str) -> Result<(), TranscriptionError> {
        let active = self
            .active
            .lock()
            .map_err(|_| TranscriptionError::Job("Transcription state is unavailable.".into()))?;
        let Some(task) = active.get(task_slot).filter(|task| task.task_id == task_id) else {
            return Err(TranscriptionError::Job(format!(
                "No matching {task_slot} transcription is running."
            )));
        };
        task.cancel.store(true, Ordering::Release);
        Ok(())
    }

    pub fn finish(
        &self,
        task_slot: &str,
        task_id: &str,
        token: &Arc<AtomicBool>,
        task_complete: bool,
    ) {
        if let Ok(mut active) = self.active.lock() {
            if active.get(task_slot).is_some_and(|current| {
                current.task_id == task_id && Arc::ptr_eq(&current.cancel, token)
            }) {
                if task_complete {
                    active.remove(task_slot);
                } else if let Some(current) = active.get_mut(task_slot) {
                    current.request_active = false;
                }
            }
        }
    }
}

const TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
const MODELS_URL: &str = "https://api.openai.com/v1/models";
const SUPPORTED_MODELS: [&str; 4] = [
    "gpt-transcribe",
    "gpt-4o-transcribe",
    "gpt-4o-mini-transcribe",
    "gpt-4o-transcribe-diarize",
];
const SUPPORTED_TRANSLATION_MODELS: [&str; 2] = ["gpt-6-luna", "gpt-6.1-sol"];
const SUPPORTED_REALTIME_MODELS: [&str; 1] = ["gpt-live-transcribe"];
const MAX_TIMEOUT_RETRIES: usize = 3;
const CONNECT_TIMEOUT_SECONDS: u64 = 30;
const STREAM_READ_TIMEOUT_SECONDS: u64 = 90;

fn is_diarization_model(model: &str) -> bool {
    model == "gpt-4o-transcribe-diarize"
}

fn format_diarized_segments(segments: &[Value]) -> Option<String> {
    let mut lines: Vec<(String, String)> = Vec::new();
    for segment in segments {
        let text = segment.get("text").and_then(Value::as_str)?.trim();
        if text.is_empty() {
            continue;
        }
        let speaker = segment
            .get("speaker")
            .or_else(|| segment.get("speaker_id"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown speaker")
            .to_owned();
        if let Some((last_speaker, last_text)) = lines.last_mut() {
            if *last_speaker == speaker {
                last_text.push(' ');
                last_text.push_str(text);
                continue;
            }
        }
        lines.push((speaker, text.to_owned()));
    }
    (!lines.is_empty()).then(|| {
        lines
            .into_iter()
            .map(|(speaker, text)| format!("{speaker}: {text}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableJob {
    pub id: String,
    pub source_name: String,
    pub model: String,
    pub completed_chunks: usize,
    pub total_chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobState {
    id: String,
    source_path: String,
    compress_audio: bool,
    #[serde(default = "default_audio_quality")]
    compression_quality: String,
    audio_stream_index: u32,
    allow_reencode: bool,
    model: String,
    parallel_requests: usize,
    prepared_parts: Vec<String>,
    completed_parts: Vec<Option<String>>,
    status: String,
    transcript: String,
    #[serde(default)]
    context_prompt: Option<String>,
}

pub(crate) fn default_audio_quality() -> String {
    "best".into()
}

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error(transparent)]
    Audio(#[from] audio::AudioError),
    #[error("Could not read the audio file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Could not connect to the OpenAI API: {0}")]
    Network(#[from] reqwest::Error),
    #[error("OpenAI API returned an error ({status}): {message}")]
    Api {
        status: u16,
        message: String,
        retry_after: Option<u64>,
    },
    #[error("This transcription model is not supported by Minefia Audio.")]
    UnsupportedModel,
    #[error("Could not read the streaming response: {0}")]
    Stream(String),
    #[error("Could not read the model list response: {0}")]
    InvalidModelList(String),
    #[error("Could not update the app with transcription progress: {0}")]
    Event(String),
    #[error("Could not save resumable transcription state: {0}")]
    Job(String),
    #[error("Transcription was canceled by the user.")]
    Canceled,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    task_id: String,
    task_slot: String,
    kind: &'static str,
    message: Option<String>,
    delta: Option<String>,
    text: Option<String>,
    chunk: Option<usize>,
    total_chunks: Option<usize>,
    prompt: Option<String>,
}

#[derive(Deserialize)]
struct ModelListResponse {
    data: Vec<ApiModel>,
}

#[derive(Deserialize)]
struct ApiModel {
    id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModels {
    transcription: Vec<String>,
    translation: Vec<String>,
    realtime: Vec<String>,
}

struct TranscriptionContext<'a> {
    app: &'a tauri::AppHandle,
    task_id: &'a str,
    task_slot: &'a str,
    client: &'a Client,
    api_key: &'a str,
    model: &'a str,
    total: usize,
}

/// Validates the credential with GET /v1/models and returns only models that
/// Minefia Audio can safely send to the transcription endpoint.
pub async fn available_models(api_key: &str) -> Result<AvailableModels, TranscriptionError> {
    let response = Client::builder()
        .build()?
        .get(MODELS_URL)
        .bearer_auth(api_key.trim())
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = api_error_message(&body)
            .unwrap_or_else(|| "The key is invalid or cannot list available models.".into());
        return Err(TranscriptionError::Api {
            status: status.as_u16(),
            message,
            retry_after: None,
        });
    }

    let response: ModelListResponse = serde_json::from_str(&body)
        .map_err(|error| TranscriptionError::InvalidModelList(error.to_string()))?;
    Ok(AvailableModels {
        transcription: filter_supported_models(&response.data, &SUPPORTED_MODELS),
        translation: filter_supported_models(&response.data, &SUPPORTED_TRANSLATION_MODELS),
        realtime: filter_supported_models(&response.data, &SUPPORTED_REALTIME_MODELS),
    })
}

pub async fn transcribe(
    app: &tauri::AppHandle,
    source: &Path,
    compress_audio: bool,
    compression_quality: &str,
    audio_stream_index: u32,
    allow_reencode: bool,
    api_key: &str,
    model: &str,
    parallel_requests: usize,
    job_id: &str,
    task_id: &str,
    task_slot: &str,
    context_prompt: Option<&str>,
    cancel: &AtomicBool,
) -> Result<String, TranscriptionError> {
    if cancel.load(Ordering::Acquire) {
        return Err(TranscriptionError::Canceled);
    }
    validate_model(model)?;
    let id = uuid::Uuid::parse_str(job_id)
        .map_err(|_| TranscriptionError::Job("The transcription job ID is invalid.".into()))?;
    let jobs_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| TranscriptionError::Job(error.to_string()))?
        .join("transcription-jobs");
    tokio::fs::create_dir_all(&jobs_dir).await?;
    let job_dir = jobs_dir.join(id.to_string());
    tokio::fs::create_dir_all(&job_dir).await?;
    let state_path = job_dir.join("job.json");
    let mut state = if tokio::fs::try_exists(&state_path).await? {
        let data = tokio::fs::read(&state_path).await?;
        let state: JobState = serde_json::from_slice(&data)
            .map_err(|error| TranscriptionError::Job(error.to_string()))?;
        if state.source_path != source.to_string_lossy()
            || state.model != model
            || state.compress_audio != compress_audio
            || (compress_audio && state.compression_quality != compression_quality)
            || state.audio_stream_index != audio_stream_index
            || state.allow_reencode != allow_reencode
            || state.context_prompt.as_deref() != context_prompt
        {
            return Err(TranscriptionError::Job(
                "This job was created for different audio settings.".into(),
            ));
        }
        state
    } else {
        emit(
            app,
            ProgressEvent::message("preparing", "Preparing audio and finding safe split points"),
            task_id,
            task_slot,
        )?;
        let prepared_dir = job_dir.join("prepared");
        tokio::fs::create_dir_all(&prepared_dir).await?;
        let parts = audio::prepare(
            source,
            compress_audio,
            compression_quality,
            audio_stream_index,
            allow_reencode,
            &prepared_dir,
        )
        .await?;
        if cancel.load(Ordering::Acquire) {
            return Err(TranscriptionError::Canceled);
        }
        if parts.is_empty() {
            return Err(TranscriptionError::Job(
                "No audio chunks were prepared.".into(),
            ));
        }
        JobState {
            id: id.to_string(),
            source_path: source.to_string_lossy().into_owned(),
            compress_audio,
            compression_quality: compression_quality.to_owned(),
            audio_stream_index,
            allow_reencode,
            model: model.to_owned(),
            parallel_requests: parallel_requests.clamp(1, 4),
            prepared_parts: parts
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            completed_parts: vec![None; parts.len()],
            status: "pending".into(),
            transcript: String::new(),
            context_prompt: context_prompt.map(str::to_owned),
        }
    };
    if state.status == "complete" {
        // Completed jobs are no longer resumable; keep only the transcript in the
        // transcript library and remove this job's recovery metadata.
        let transcript = state.transcript;
        remove_completed_job_dir(&jobs_dir, &job_dir).await;
        return Ok(transcript);
    }
    state.status = "running".into();
    save_job(&state_path, &state).await?;
    let total = state.prepared_parts.len();
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECONDS))
        .read_timeout(Duration::from_secs(STREAM_READ_TIMEOUT_SECONDS))
        .build()?;
    let request_context = TranscriptionContext {
        app,
        task_id,
        task_slot,
        client: &client,
        api_key,
        model,
        total,
    };
    let pending = state
        .prepared_parts
        .iter()
        .enumerate()
        .filter(|(index, _)| state.completed_parts[*index].is_none())
        .map(|(index, path)| (index, PathBuf::from(path)))
        .collect::<Vec<_>>();
    for batch in pending.chunks(state.parallel_requests.clamp(1, 4)) {
        let mut requests = Vec::with_capacity(batch.len());
        for (index, path) in batch {
            let index = *index;
            let chunk = index + 1;
            emit(
                app,
                ProgressEvent::chunk("uploading", chunk, total),
                task_id,
                task_slot,
            )?;
            let prefix = transcript_prefix(&state.completed_parts, index);
            let previous_context = (!prefix.is_empty()).then(|| tail_chars(&prefix, 900));
            let prompt =
                compose_prompt(state.context_prompt.as_deref(), previous_context.as_deref());
            let request_context = &request_context;
            let path = path.clone();
            requests.push(async move {
                let result = transcribe_part_with_retry(
                    request_context,
                    &path,
                    prompt.as_deref(),
                    chunk,
                    cancel,
                )
                .await;
                (index, result)
            });
        }
        let results = join_all(requests).await;
        let mut first_error = None;
        for (index, result) in results {
            match result {
                Ok(text) => {
                    state.completed_parts[index] = Some(text);
                    state.transcript = transcript_prefix(&state.completed_parts, total);
                    save_job(&state_path, &state).await?;
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if cancel.load(Ordering::Acquire) {
            state.status = "interrupted".into();
            save_job(&state_path, &state).await?;
            return Err(TranscriptionError::Canceled);
        }
        if let Some(error) = first_error {
            state.status = "interrupted".into();
            save_job(&state_path, &state).await?;
            return Err(error);
        }
    }
    state.status = "complete".into();
    state.transcript = transcript_prefix(&state.completed_parts, total);
    save_job(&state_path, &state).await?;

    emit(
        app,
        ProgressEvent::message("done", "Transcription complete"),
        task_id,
        task_slot,
    )?;
    // A completed transcript is persisted separately. The job directory is only
    // recovery state for interrupted work, so it can be removed after completion.
    remove_completed_job_dir(&jobs_dir, &job_dir).await;
    Ok(state.transcript)
}

async fn remove_completed_job_dir(jobs_dir: &Path, job_dir: &Path) {
    // Remove only the per-job child directory; retain the shared jobs root.
    let _ = tokio::fs::remove_dir_all(job_dir).await;
    let _ = tokio::fs::create_dir_all(jobs_dir).await;
}

pub async fn resume_job(
    app: &tauri::AppHandle,
    api_key: &str,
    job_id: &str,
    task_id: &str,
    task_slot: &str,
    cancel: &AtomicBool,
) -> Result<String, TranscriptionError> {
    let id = uuid::Uuid::parse_str(job_id)
        .map_err(|_| TranscriptionError::Job("The transcription job ID is invalid.".into()))?;
    let state_path = app
        .path()
        .app_data_dir()
        .map_err(|error| TranscriptionError::Job(error.to_string()))?
        .join("transcription-jobs")
        .join(id.to_string())
        .join("job.json");
    let data = tokio::fs::read(&state_path).await?;
    let state: JobState = serde_json::from_slice(&data)
        .map_err(|error| TranscriptionError::Job(error.to_string()))?;
    transcribe(
        app,
        Path::new(&state.source_path),
        state.compress_audio,
        &state.compression_quality,
        state.audio_stream_index,
        state.allow_reencode,
        api_key,
        &state.model,
        state.parallel_requests,
        &state.id,
        task_id,
        task_slot,
        state.context_prompt.as_deref(),
        cancel,
    )
    .await
}

pub async fn resumable_jobs(
    app: &tauri::AppHandle,
) -> Result<Vec<ResumableJob>, TranscriptionError> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| TranscriptionError::Job(error.to_string()))?
        .join("transcription-jobs");
    if !tokio::fs::try_exists(&root).await? {
        return Ok(Vec::new());
    }
    let mut dir = tokio::fs::read_dir(root).await?;
    let mut jobs = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path().join("job.json");
        let Ok(data) = tokio::fs::read(path).await else {
            continue;
        };
        let Ok(state) = serde_json::from_slice::<JobState>(&data) else {
            continue;
        };
        if state.status == "complete" {
            continue;
        }
        let source_name = Path::new(&state.source_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        jobs.push(ResumableJob {
            id: state.id,
            source_name,
            model: state.model,
            completed_chunks: state
                .completed_parts
                .iter()
                .filter(|item| item.is_some())
                .count(),
            total_chunks: state.prepared_parts.len(),
        });
    }
    Ok(jobs)
}

async fn save_job(path: &Path, state: &JobState) -> Result<(), TranscriptionError> {
    let temporary = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(state)
        .map_err(|error| TranscriptionError::Job(error.to_string()))?;
    tokio::fs::write(&temporary, data).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

fn transcript_prefix(parts: &[Option<String>], end: usize) -> String {
    parts
        .iter()
        .take(end)
        .filter_map(|part| part.as_deref())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn compose_prompt(
    video_context: Option<&str>,
    previous_transcript: Option<&str>,
) -> Option<String> {
    let mut sections = Vec::new();
    if let Some(context) = video_context.filter(|text| !text.trim().is_empty()) {
        sections.push(context.chars().take(1100).collect::<String>());
    }
    if let Some(previous) = previous_transcript.filter(|text| !text.trim().is_empty()) {
        sections.push(format!(
            "Previous transcript context: {}",
            previous.chars().take(800).collect::<String>()
        ));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

/// Transcribes one already-prepared Standard Mode chunk. Chunk concurrency and
/// chronological assembly are owned by the system-audio session coordinator.
pub async fn transcribe_recording_part(
    app: &tauri::AppHandle,
    api_key: &str,
    path: &Path,
    model: &str,
    prompt: Option<&str>,
    chunk: usize,
) -> Result<String, TranscriptionError> {
    validate_model(model)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECONDS))
        .read_timeout(Duration::from_secs(STREAM_READ_TIMEOUT_SECONDS))
        .build()?;

    for retry in 0..=MAX_TIMEOUT_RETRIES {
        let bytes = tokio::fs::read(path).await?;
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let file_part = multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime_for_path(path))?;
        let diarize = is_diarization_model(&model);
        let actual_prompt = if diarize { None } else { prompt };
        emit(
            app,
            ProgressEvent::prompt(chunk, None, actual_prompt),
            "audio-capture",
            "system",
        )?;
        let mut form = multipart::Form::new()
            .text("model", model.to_owned())
            .text("stream", "true")
            .text("chunking_strategy", "auto")
            .part("file", file_part);
        if diarize {
            form = form.text("response_format", "diarized_json");
        }
        if let Some(prompt) = actual_prompt {
            form = form.text("prompt", prompt.to_owned());
        }

        match client
            .post(TRANSCRIPTIONS_URL)
            .bearer_auth(api_key)
            .multipart(form)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let retry_after = retry_after_seconds(response.headers());
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    let error = TranscriptionError::Api {
                        status: status.as_u16(),
                        message: api_error_message(&body)
                            .unwrap_or_else(|| body.chars().take(500).collect()),
                        retry_after,
                    };
                    if error.is_retryable() && retry < MAX_TIMEOUT_RETRIES {
                        sleep(Duration::from_secs(error.retry_delay(retry))).await;
                        continue;
                    }
                    return Err(error);
                }

                let mut stream = response.bytes_stream().eventsource();
                let mut streamed_text = String::new();
                let mut completed_text = None;
                let mut diarized_segments = Vec::new();
                while let Some(event) = stream.next().await {
                    let event = match event {
                        Ok(event) => event,
                        Err(EventStreamError::Transport(error)) => {
                            return Err(TranscriptionError::Network(error));
                        }
                        Err(error) => {
                            return Err(TranscriptionError::Stream(error.to_string()));
                        }
                    };
                    if event.data == "[DONE]" {
                        break;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                        continue;
                    };
                    match value.get("type").and_then(Value::as_str) {
                        Some("transcript.text.delta") => {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                                streamed_text.push_str(delta);
                                emit(
                                    app,
                                    ProgressEvent::delta(delta, chunk, 0),
                                    "audio-capture",
                                    "system",
                                )?;
                            }
                        }
                        Some("transcript.text.done") => {
                            completed_text =
                                value.get("text").and_then(Value::as_str).map(str::to_owned);
                        }
                        Some("transcript.text.segment") if diarize => {
                            let segment = value.get("segment").unwrap_or(&value);
                            diarized_segments.push(segment.clone());
                            if let Some(text) = segment.get("text").and_then(Value::as_str) {
                                let speaker = segment
                                    .get("speaker")
                                    .or_else(|| segment.get("speaker_id"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("Unknown speaker");
                                let delta = format!("{speaker}: {text}\n");
                                streamed_text.push_str(&delta);
                                emit(
                                    app,
                                    ProgressEvent::delta(&delta, chunk, 0),
                                    "audio-capture",
                                    "system",
                                )?;
                            }
                        }
                        _ => {}
                    }
                }
                let result = if diarize {
                    format_diarized_segments(&diarized_segments)
                        .or(completed_text)
                        .unwrap_or_else(|| streamed_text.clone())
                } else {
                    completed_text.unwrap_or_else(|| streamed_text.clone())
                };
                if streamed_text.is_empty() && !result.is_empty() {
                    emit(
                        app,
                        ProgressEvent::delta(&result, chunk, 0),
                        "audio-capture",
                        "system",
                    )?;
                }
                return Ok(result);
            }
            Err(error)
                if (error.is_timeout() || error.is_connect()) && retry < MAX_TIMEOUT_RETRIES =>
            {
                sleep(Duration::from_secs(retry_delay_seconds(retry))).await;
            }
            Err(error) => return Err(TranscriptionError::Network(error)),
        }
    }
    unreachable!("the retry loop always returns")
}

async fn transcribe_part_with_retry(
    context: &TranscriptionContext<'_>,
    path: &Path,
    prompt: Option<&str>,
    chunk: usize,
    cancel: &AtomicBool,
) -> Result<String, TranscriptionError> {
    for retry in 0..=MAX_TIMEOUT_RETRIES {
        if cancel.load(Ordering::Acquire) {
            return Err(TranscriptionError::Canceled);
        }
        match transcribe_part_once(context, path, prompt, chunk, cancel).await {
            Ok(text) => {
                emit(
                    context.app,
                    ProgressEvent::chunk_result(chunk, context.total, &text),
                    context.task_id,
                    context.task_slot,
                )?;
                return Ok(text);
            }
            Err(error) if error.is_retryable() && retry < MAX_TIMEOUT_RETRIES => {
                let delay = error.retry_delay(retry);
                emit(
                    context.app,
                    ProgressEvent::retry(
                        chunk,
                        context.total,
                        &format!(
                            "Part {chunk} hit a temporary API or network limit. Retrying in {delay}s ({}/{MAX_TIMEOUT_RETRIES})",
                            retry + 1
                        ),
                    ),
                    context.task_id,
                    context.task_slot,
                )?;
                tokio::select! {
                    _ = sleep(Duration::from_secs(delay)) => {},
                    _ = wait_for_cancel(cancel) => return Err(TranscriptionError::Canceled),
                }
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the retry loop always returns on success or final failure")
}

async fn transcribe_part_once(
    context: &TranscriptionContext<'_>,
    path: &Path,
    prompt: Option<&str>,
    chunk: usize,
    cancel: &AtomicBool,
) -> Result<String, TranscriptionError> {
    let bytes = tokio::fs::read(path).await?;
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let file_part = multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(mime_for_path(path))?;
    let diarize = is_diarization_model(&context.model);
    let actual_prompt = if diarize { None } else { prompt };
    emit(
        context.app,
        ProgressEvent::prompt(chunk, Some(context.total), actual_prompt),
        context.task_id,
        context.task_slot,
    )?;
    let mut form = multipart::Form::new()
        .text("model", context.model.to_owned())
        .text("stream", "true")
        .text("chunking_strategy", "auto")
        .part("file", file_part);
    if diarize {
        form = form.text("response_format", "diarized_json");
    } else if let Some(prompt) = actual_prompt {
        form = form.text("prompt", prompt.to_owned());
    }

    let request = context
        .client
        .post(TRANSCRIPTIONS_URL)
        .bearer_auth(context.api_key)
        .multipart(form);
    let response = tokio::select! {
        _ = wait_for_cancel(cancel) => return Err(TranscriptionError::Canceled),
        response = request.send() => response.map_err(TranscriptionError::Network)?,
    };
    let status = response.status();
    if !status.is_success() {
        let retry_after = retry_after_seconds(response.headers());
        let body = response.text().await.unwrap_or_default();
        let message = api_error_message(&body).unwrap_or_else(|| body.chars().take(500).collect());
        return Err(TranscriptionError::Api {
            status: status.as_u16(),
            message,
            retry_after,
        });
    }

    let mut stream = response.bytes_stream().eventsource();
    let mut streamed_text = String::new();
    let mut completed_text: Option<String> = None;
    let mut diarized_segments = Vec::new();

    loop {
        let next = tokio::select! {
            _ = wait_for_cancel(cancel) => return Err(TranscriptionError::Canceled),
            next = stream.next() => next,
        };
        let Some(event) = next else {
            break;
        };
        let event = match event {
            Ok(event) => event,
            Err(EventStreamError::Transport(error)) => {
                return Err(TranscriptionError::Network(error));
            }
            Err(error) => return Err(TranscriptionError::Stream(error.to_string())),
        };
        if event.data == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("transcript.text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    streamed_text.push_str(delta);
                    emit(
                        context.app,
                        ProgressEvent::delta(delta, chunk, context.total),
                        context.task_id,
                        context.task_slot,
                    )?;
                }
            }
            Some("transcript.text.done") => {
                completed_text = value.get("text").and_then(Value::as_str).map(str::to_owned);
            }
            Some("transcript.text.segment") if diarize => {
                let segment = value.get("segment").unwrap_or(&value);
                diarized_segments.push(segment.clone());
            }
            _ => {}
        }
    }

    let result = if diarize {
        format_diarized_segments(&diarized_segments)
            .or(completed_text)
            .unwrap_or_else(|| streamed_text.clone())
    } else {
        completed_text.unwrap_or_else(|| streamed_text.clone())
    };
    if streamed_text.is_empty() && !result.is_empty() {
        emit(
            context.app,
            ProgressEvent::delta(&result, chunk, context.total),
            context.task_id,
            context.task_slot,
        )?;
    }
    Ok(result)
}

async fn wait_for_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl TranscriptionError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Network(error) if error.is_timeout() || error.is_connect())
            || matches!(
                self,
                Self::Api {
                    status: 408 | 429 | 500..=599,
                    ..
                }
            )
    }

    fn retry_delay(&self, retry: usize) -> u64 {
        let exponential = retry_delay_seconds(retry);
        match self {
            Self::Api {
                retry_after: Some(seconds),
                ..
            } => (*seconds).clamp(exponential, 900),
            _ => exponential,
        }
    }
}

fn retry_delay_seconds(retry: usize) -> u64 {
    2_u64.pow(retry as u32 + 1)
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.parse::<u64>().unwrap_or(60))
}

fn validate_model(model: &str) -> Result<(), TranscriptionError> {
    if SUPPORTED_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(TranscriptionError::UnsupportedModel)
    }
}

fn filter_supported_models(models: &[ApiModel], supported_models: &[&str]) -> Vec<String> {
    // Keep the UI order stable while treating the API response as the source
    // of truth for which transcription models this credential can access.
    supported_models
        .iter()
        .filter(|supported| models.iter().any(|model| model.id.as_str() == **supported))
        .map(|model| (*model).to_owned())
        .collect()
}

fn api_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .pointer("/error/message")?
        .as_str()
        .map(str::to_owned)
}

fn emit(
    app: &tauri::AppHandle,
    mut event: ProgressEvent,
    task_id: &str,
    task_slot: &str,
) -> Result<(), TranscriptionError> {
    event.task_id = task_id.to_owned();
    event.task_slot = task_slot.to_owned();
    app.emit("transcription-progress", event)
        .map_err(|error| TranscriptionError::Event(error.to_string()))
}

fn tail_chars(value: &str, count: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(count).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "flac" => "audio/flac",
        "mp3" | "mpga" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        "mpeg" => "video/mpeg",
        _ => "application/octet-stream",
    }
}

impl ProgressEvent {
    fn message(kind: &'static str, message: &str) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind,
            message: Some(message.into()),
            delta: None,
            text: None,
            chunk: None,
            total_chunks: None,
            prompt: None,
        }
    }

    fn chunk(kind: &'static str, chunk: usize, total_chunks: usize) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind,
            message: None,
            delta: None,
            text: None,
            chunk: Some(chunk),
            total_chunks: Some(total_chunks),
            prompt: None,
        }
    }

    fn delta(delta: &str, chunk: usize, total_chunks: usize) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind: "delta",
            message: None,
            delta: Some(delta.into()),
            text: None,
            chunk: Some(chunk),
            total_chunks: Some(total_chunks),
            prompt: None,
        }
    }

    fn retry(chunk: usize, total_chunks: usize, message: &str) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind: "retrying",
            message: Some(message.into()),
            delta: None,
            text: None,
            chunk: Some(chunk),
            total_chunks: Some(total_chunks),
            prompt: None,
        }
    }

    fn chunk_result(chunk: usize, total_chunks: usize, text: &str) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind: "chunk_done",
            message: None,
            delta: None,
            text: Some(text.to_owned()),
            chunk: Some(chunk),
            total_chunks: Some(total_chunks),
            prompt: None,
        }
    }

    fn prompt(chunk: usize, total_chunks: Option<usize>, prompt: Option<&str>) -> Self {
        Self {
            task_id: String::new(),
            task_slot: String::new(),
            kind: "prompt",
            message: None,
            delta: None,
            text: None,
            chunk: Some(chunk),
            total_chunks,
            prompt: prompt.map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compose_prompt, filter_supported_models, format_diarized_segments, retry_delay_seconds,
        ApiModel, TranscriptionManager,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn file_and_online_transcriptions_have_independent_cancellation_slots() {
        let manager = TranscriptionManager::default();
        let file = manager.begin("file", "file-task").unwrap();
        let online = manager.begin("online", "online-task").unwrap();

        assert!(manager.begin("file", "duplicate-file-task").is_err());
        assert!(manager.begin("file", "file-task").is_err());
        assert!(manager.cancel("file", "online-task").is_err());
        manager.cancel("online", "online-task").unwrap();
        assert!(!file.load(Ordering::Acquire));
        assert!(online.load(Ordering::Acquire));

        manager.finish("file", "file-task", &file, false);
        let next_file_request = manager.begin("file", "file-task").unwrap();
        assert!(std::sync::Arc::ptr_eq(&file, &next_file_request));
        manager.finish("file", "file-task", &file, true);
        manager.finish("online", "online-task", &online, true);
        assert!(manager.begin("file", "next-file-task").is_ok());
    }

    #[test]
    fn filters_and_orders_available_transcription_models() {
        let models = vec![
            ApiModel {
                id: "gpt-4o-mini-transcribe".into(),
            },
            ApiModel {
                id: "gpt-4.1".into(),
            },
            ApiModel {
                id: "gpt-transcribe".into(),
            },
            ApiModel {
                id: "gpt-4o-transcribe-diarize".into(),
            },
        ];

        assert_eq!(
            filter_supported_models(&models, &super::SUPPORTED_MODELS),
            vec![
                "gpt-transcribe".to_owned(),
                "gpt-4o-mini-transcribe".to_owned(),
                "gpt-4o-transcribe-diarize".to_owned()
            ]
        );
    }

    #[test]
    fn diarized_segments_keep_speaker_labels_and_merge_adjacent_segments() {
        let segments = vec![
            serde_json::json!({"speaker":"speaker_0", "text":"Hello"}),
            serde_json::json!({"speaker":"speaker_0", "text":"there."}),
            serde_json::json!({"speaker":"speaker_1", "text":"Hi."}),
        ];
        assert_eq!(
            format_diarized_segments(&segments).as_deref(),
            Some("speaker_0: Hello there.\nspeaker_1: Hi.")
        );
    }

    #[test]
    fn timeout_retries_use_bounded_exponential_delays() {
        assert_eq!(
            (0..3).map(retry_delay_seconds).collect::<Vec<_>>(),
            vec![2, 4, 8]
        );
    }

    #[test]
    fn prompt_keeps_video_metadata_and_prior_chunk_context() {
        let prompt = compose_prompt(
            Some("YouTube title: Example\nDescription: Context"),
            Some("Previous words"),
        )
        .expect("prompt should be composed");
        assert!(prompt.contains("YouTube title: Example"));
        assert!(prompt.contains("Description: Context"));
        assert!(prompt.contains("Previous transcript context: Previous words"));
    }
}
