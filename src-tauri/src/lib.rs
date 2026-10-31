mod admin_credentials;
mod app_project;
mod audio;
mod browser_media;
mod credentials;
mod errors;
mod library;
mod online_media;
mod pricing;
mod realtime;
mod storage;
mod summarization;
mod system_audio;
mod technical_log;
mod tooling;
mod transcript_store;
mod transcription;
mod translation;
mod usage;

use audio::AudioInfo;
use serde::Deserialize;
use std::{path::Path, sync::Arc};
use tauri::Manager;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscribeRequest {
    path: String,
    compress_audio: bool,
    #[serde(default = "transcription::default_audio_quality")]
    audio_quality: String,
    audio_stream_index: u32,
    allow_reencode: bool,
    model: String,
    parallel_requests: usize,
    job_id: String,
    task_id: String,
    task_slot: String,
    task_complete: bool,
    #[serde(default)]
    context_prompt: Option<String>,
}

#[tauri::command]
async fn inspect_audio(path: String) -> Result<AudioInfo, errors::AppError> {
    audio::inspect(Path::new(&path))
        .await
        .map_err(errors::classify)
}

#[tauri::command]
fn allow_media_preview(app: tauri::AppHandle, path: String) -> Result<(), errors::AppError> {
    let path = Path::new(&path);
    if !path.is_file() {
        return Err(errors::classify(
            "The media preview file could not be found.",
        ));
    }
    app.asset_protocol_scope()
        .allow_file(path)
        .map_err(errors::classify)
}

#[tauri::command]
fn append_technical_log(
    app: tauri::AppHandle,
    entry: technical_log::TechnicalLogEntry,
) -> Result<(), errors::AppError> {
    technical_log::append(&app, entry).map_err(errors::classify)
}

#[tauri::command]
async fn inspect_online_media(
    app: tauri::AppHandle,
    manager: tauri::State<'_, online_media::OnlineMediaManager>,
    url: String,
    headless_mode: bool,
) -> Result<online_media::OnlineMediaInfo, errors::AppError> {
    let cancel = manager.begin().map_err(errors::classify)?;
    let result = online_media::inspect(&app, &url, headless_mode, &cancel).await;
    manager.finish(&cancel);
    result.map_err(errors::classify)
}

#[tauri::command]
async fn download_online_caption(
    url: String,
    language: String,
    automatic: bool,
) -> Result<String, errors::AppError> {
    online_media::download_caption(&url, &language, automatic)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn inspect_web_page(
    app: tauri::AppHandle,
    manager: tauri::State<'_, online_media::OnlineMediaManager>,
    url: String,
    headless_mode: bool,
) -> Result<browser_media::PageMediaInfo, errors::AppError> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| errors::classify(error.to_string()))?;
    let cancel = manager.begin().map_err(errors::classify)?;
    let profile_dir = data_dir.join("browser-profile");
    let chromium_dir = data_dir.join("browser-runtime");
    let task_cancel = Arc::clone(&cancel);
    let result = match tokio::task::spawn_blocking(move || {
        browser_media::inspect(
            url,
            profile_dir,
            chromium_dir,
            headless_mode,
            task_cancel.as_ref(),
        )
    })
    .await
    {
        Ok(result) => result.map_err(errors::classify),
        Err(error) => Err(errors::classify(format!(
            "Browser inspection failed: {error}"
        ))),
    };
    manager.finish(&cancel);
    result
}

#[tauri::command]
async fn extract_online_media(
    app: tauri::AppHandle,
    manager: tauri::State<'_, online_media::OnlineMediaManager>,
    request: online_media::ExtractRequest,
) -> Result<AudioInfo, errors::AppError> {
    let cancel = manager.begin().map_err(errors::classify)?;
    let result = online_media::extract(&app, request, &cancel).await;
    manager.finish(&cancel);
    result.map_err(errors::classify)
}

#[tauri::command]
fn cancel_online_media(
    manager: tauri::State<'_, online_media::OnlineMediaManager>,
) -> Result<(), errors::AppError> {
    manager.cancel().map_err(errors::classify)
}

#[tauri::command]
fn list_api_keys(
    app: tauri::AppHandle,
) -> Result<Vec<credentials::ApiKeySummary>, errors::AppError> {
    credentials::list(&app).map_err(errors::classify)
}

#[tauri::command]
async fn save_api_key(
    app: tauri::AppHandle,
    label: String,
    api_key: String,
) -> Result<credentials::ApiKeySummary, errors::AppError> {
    transcription::available_models(&api_key)
        .await
        .map_err(errors::classify)?;
    credentials::save(&app, &label, &api_key).map_err(errors::classify)
}

#[tauri::command]
async fn list_available_models(
    app: tauri::AppHandle,
) -> Result<transcription::AvailableModels, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    transcription::available_models(&api_key)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
fn list_audio_input_devices() -> Result<Vec<system_audio::InputDevice>, errors::AppError> {
    system_audio::list_input_devices().map_err(errors::classify)
}

#[tauri::command]
fn select_api_key(app: tauri::AppHandle, id: String) -> Result<(), errors::AppError> {
    credentials::select(&app, &id).map_err(errors::classify)
}

#[tauri::command]
fn delete_api_key(app: tauri::AppHandle, id: String) -> Result<(), errors::AppError> {
    credentials::remove(&app, &id).map_err(errors::classify)
}

#[tauri::command]
async fn write_transcript(path: String, content: String) -> Result<(), errors::AppError> {
    tokio::fs::write(path, content)
        .await
        .map_err(|error| errors::classify(format!("Could not save transcript: {error}")))
}

#[tauri::command]
async fn append_saved_transcript(
    app: tauri::AppHandle,
    session_id: String,
    chunk: usize,
    attempt: usize,
    name: String,
    text: String,
) -> Result<(), errors::AppError> {
    transcript_store::append(&app, &session_id, chunk, attempt, &name, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn replace_saved_transcript_chunk(
    app: tauri::AppHandle,
    session_id: String,
    chunk: usize,
    attempt: usize,
    name: String,
    text: String,
) -> Result<(), errors::AppError> {
    transcript_store::replace_chunk(&app, &session_id, chunk, attempt, &name, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn save_transcript_summary(
    app: tauri::AppHandle,
    session_id: String,
    name: String,
    text: String,
) -> Result<(), errors::AppError> {
    transcript_store::save_summary(&app, &session_id, &name, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn save_transcript_notes(
    app: tauri::AppHandle,
    session_id: String,
    name: String,
    text: String,
    note_style: String,
) -> Result<(), errors::AppError> {
    transcript_store::save_notes(&app, &session_id, &name, &text, &note_style)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn fetch_project_costs(
    app: tauri::AppHandle,
) -> Result<usage::ProjectCosts, errors::AppError> {
    if !app_project::usage_ready(&app).map_err(errors::classify)? {
        return Err(errors::classify(
            "Usage is available after Minefia Audio is linked to a Project Service Account key.",
        ));
    }
    let api_key = admin_credentials::load_selected(&app).map_err(errors::classify)?;
    let project_ids = app_project::usage_project_ids(&app).map_err(errors::classify)?;
    if project_ids.is_empty() {
        return Err(errors::classify(
            "No Minefia Audio Service Account project is linked.",
        ));
    }
    usage::fetch(&api_key, &project_ids)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
fn list_admin_api_keys(
    app: tauri::AppHandle,
) -> Result<Vec<admin_credentials::AdminKeySummary>, errors::AppError> {
    admin_credentials::list(&app).map_err(errors::classify)
}

#[tauri::command]
async fn save_admin_api_key(
    app: tauri::AppHandle,
    label: String,
    api_key: String,
) -> Result<admin_credentials::AdminKeySummary, errors::AppError> {
    // Verify the Admin credential with the lightweight admin-key listing route.
    usage::validate_admin_key(&api_key)
        .await
        .map_err(errors::classify)?;
    admin_credentials::save(&app, &label, &api_key).map_err(errors::classify)
}

#[tauri::command]
fn get_minefia_projects(
    app: tauri::AppHandle,
) -> Result<Vec<app_project::MinefiaProject>, errors::AppError> {
    app_project::list(&app).map_err(errors::classify)
}

#[tauri::command]
async fn list_openai_projects(
    app: tauri::AppHandle,
) -> Result<Vec<app_project::ProjectSummary>, errors::AppError> {
    let admin_key = admin_credentials::load_selected(&app).map_err(errors::classify)?;
    app_project::list_projects(&admin_key)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
fn minefia_usage_ready(app: tauri::AppHandle) -> Result<bool, errors::AppError> {
    app_project::usage_ready(&app).map_err(errors::classify)
}

#[tauri::command]
async fn create_minefia_project_key(
    app: tauri::AppHandle,
    existing_project_id: Option<String>,
    project_name: String,
    service_account_name: String,
) -> Result<app_project::CreatedProjectKey, errors::AppError> {
    let admin_key = admin_credentials::load_selected(&app).map_err(errors::classify)?;
    app_project::create_project_and_key(
        &app,
        &admin_key,
        existing_project_id.as_deref(),
        &project_name,
        &service_account_name,
    )
    .await
    .map_err(errors::classify)
}

#[tauri::command]
fn select_admin_api_key(app: tauri::AppHandle, id: String) -> Result<(), errors::AppError> {
    admin_credentials::select(&app, &id).map_err(errors::classify)
}

#[tauri::command]
fn delete_admin_api_key(app: tauri::AppHandle, id: String) -> Result<(), errors::AppError> {
    admin_credentials::remove(&app, &id).map_err(errors::classify)
}

#[tauri::command]
async fn save_transcript_translation(
    app: tauri::AppHandle,
    session_id: String,
    source_name: String,
    language: String,
    text: String,
) -> Result<(), errors::AppError> {
    transcript_store::save_translation(&app, &session_id, &source_name, &language, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn save_summary_translation(
    app: tauri::AppHandle,
    session_id: String,
    source_name: String,
    language: String,
    text: String,
) -> Result<(), errors::AppError> {
    transcript_store::save_summary_translation(&app, &session_id, &source_name, &language, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn save_notes_translation(
    app: tauri::AppHandle,
    session_id: String,
    source_name: String,
    language: String,
    text: String,
    note_style: String,
) -> Result<(), errors::AppError> {
    transcript_store::save_notes_translation(
        &app,
        &session_id,
        &source_name,
        &language,
        &text,
        &note_style,
    )
    .await
    .map_err(errors::classify)
}

#[tauri::command]
async fn snapshot_saved_transcript(
    app: tauri::AppHandle,
    session_id: String,
    name: String,
) -> Result<String, errors::AppError> {
    transcript_store::snapshot_transcript(&app, &session_id, &name)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(errors::classify)
}

#[tauri::command]
async fn replace_final_transcript(
    app: tauri::AppHandle,
    session_id: String,
    name: String,
    text: String,
) -> Result<String, errors::AppError> {
    transcript_store::replace_final_transcript(&app, &session_id, &name, &text)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(errors::classify)
}

#[tauri::command]
async fn save_enhanced_transcript(
    app: tauri::AppHandle,
    session_id: String,
    name: String,
    text: String,
) -> Result<String, errors::AppError> {
    transcript_store::save_enhanced_transcript(&app, &session_id, &name, &text)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(errors::classify)
}

#[tauri::command]
async fn cleanup_saved_transcript_chunks(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<(), errors::AppError> {
    transcript_store::cleanup_chunk_transcripts(&app, &session_id)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn list_saved_transcripts(
    app: tauri::AppHandle,
) -> Result<Vec<transcript_store::SavedTranscript>, errors::AppError> {
    transcript_store::list(&app).await.map_err(errors::classify)
}

#[tauri::command]
async fn list_library_files(
    app: tauri::AppHandle,
) -> Result<Vec<library::LibraryFile>, errors::AppError> {
    library::list(&app).await.map_err(errors::classify)
}

#[tauri::command]
async fn delete_library_file(app: tauri::AppHandle, path: String) -> Result<(), errors::AppError> {
    library::remove(&app, &path).await.map_err(errors::classify)
}

#[tauri::command]
async fn reveal_library_path(app: tauri::AppHandle, path: String) -> Result<(), errors::AppError> {
    library::reveal(&app, &path).await.map_err(errors::classify)
}

#[tauri::command]
async fn import_local_media(
    app: tauri::AppHandle,
    path: String,
) -> Result<library::LibraryFile, errors::AppError> {
    library::import_local(&app, &path)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn extract_local_audio(
    app: tauri::AppHandle,
    path: String,
    audio_stream_index: u32,
) -> Result<String, errors::AppError> {
    let source = Path::new(&path);
    let recordings = storage::extracted_media_root(&app).map_err(errors::classify)?;
    audio::extract_local_track(source, audio_stream_index, &recordings)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| errors::classify(error.to_string()))
}

#[tauri::command]
async fn delete_saved_transcript(
    app: tauri::AppHandle,
    id: String,
) -> Result<(), errors::AppError> {
    transcript_store::remove(&app, &id)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn translate_transcript(
    app: tauri::AppHandle,
    text: String,
    target_language: String,
    model: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    translation::translate(&app, &api_key, &text, &target_language, &model)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn update_summary_notes(
    app: tauri::AppHandle,
    previous_notes: String,
    new_transcript: String,
    model: String,
    note_style: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    summarization::append_notes(
        &app,
        &api_key,
        &previous_notes,
        &new_transcript,
        &model,
        &note_style,
    )
    .await
    .map_err(errors::classify)
}

#[tauri::command]
async fn summarize_transcript(
    app: tauri::AppHandle,
    text: String,
    model: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    summarization::summarize_transcript(&app, &api_key, &text, &model)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn generate_final_notes(
    app: tauri::AppHandle,
    text: String,
    model: String,
    note_style: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    summarization::generate_final_notes(&app, &api_key, &text, &model, &note_style)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn ask_about_transcript(
    app: tauri::AppHandle,
    transcript: String,
    summary: String,
    question: String,
    conversation: String,
    model: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    summarization::answer_question(
        &app,
        &api_key,
        &transcript,
        &summary,
        &question,
        &conversation,
        &model,
    )
    .await
    .map_err(errors::classify)
}

#[tauri::command]
async fn enhance_transcript(
    app: tauri::AppHandle,
    text: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    translation::enhance_transcript(&app, &api_key, &text)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn transcribe_audio(
    app: tauri::AppHandle,
    manager: tauri::State<'_, transcription::TranscriptionManager>,
    request: TranscribeRequest,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    let cancel = manager
        .begin(&request.task_slot, &request.task_id)
        .map_err(errors::classify)?;
    let result = transcription::transcribe(
        &app,
        Path::new(&request.path),
        request.compress_audio,
        &request.audio_quality,
        request.audio_stream_index,
        request.allow_reencode,
        &api_key,
        &request.model,
        request.parallel_requests,
        &request.job_id,
        &request.task_id,
        &request.task_slot,
        request.context_prompt.as_deref(),
        &cancel,
    )
    .await;
    manager.finish(
        &request.task_slot,
        &request.task_id,
        &cancel,
        request.task_complete || result.is_err(),
    );
    result.map_err(errors::classify)
}

#[tauri::command]
async fn list_resumable_transcriptions(
    app: tauri::AppHandle,
) -> Result<Vec<transcription::ResumableJob>, errors::AppError> {
    transcription::resumable_jobs(&app)
        .await
        .map_err(errors::classify)
}

#[tauri::command]
async fn resume_transcription(
    app: tauri::AppHandle,
    manager: tauri::State<'_, transcription::TranscriptionManager>,
    job_id: String,
    task_id: String,
    task_slot: String,
) -> Result<String, errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    let cancel = manager
        .begin(&task_slot, &task_id)
        .map_err(errors::classify)?;
    let result =
        transcription::resume_job(&app, &api_key, &job_id, &task_id, &task_slot, &cancel).await;
    manager.finish(&task_slot, &task_id, &cancel, true);
    result.map_err(errors::classify)
}

#[tauri::command]
fn cancel_transcription(
    manager: tauri::State<'_, transcription::TranscriptionManager>,
    task_id: String,
    task_slot: String,
) -> Result<(), errors::AppError> {
    manager
        .cancel(&task_slot, &task_id)
        .map_err(errors::classify)
}

#[tauri::command]
fn start_system_audio(
    app: tauri::AppHandle,
    manager: tauri::State<'_, system_audio::SystemAudioManager>,
    model: String,
    source: String,
    input_device: Option<String>,
    parallel_requests: usize,
    quality: String,
    compact_microphone: bool,
    compact_system_audio: bool,
) -> Result<(), errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    system_audio::start(
        app,
        &manager,
        api_key,
        model,
        source,
        input_device,
        parallel_requests,
        quality,
        compact_microphone,
        compact_system_audio,
    )
    .map_err(errors::classify)
}

#[tauri::command]
fn start_realtime_audio(
    app: tauri::AppHandle,
    manager: tauri::State<'_, realtime::RealtimeManager>,
    source: String,
    input_device: Option<String>,
    quality: String,
    compact_microphone: bool,
    compact_system_audio: bool,
) -> Result<(), errors::AppError> {
    let api_key = credentials::load_selected(&app).map_err(errors::classify)?;
    realtime::start(
        app,
        &manager,
        api_key,
        source,
        input_device,
        quality,
        compact_microphone,
        compact_system_audio,
    )
    .map_err(errors::classify)
}

#[tauri::command]
fn stop_realtime_audio(
    manager: tauri::State<'_, realtime::RealtimeManager>,
) -> Result<(), errors::AppError> {
    realtime::stop(&manager).map_err(errors::classify)
}

#[tauri::command]
fn stop_system_audio(
    manager: tauri::State<'_, system_audio::SystemAudioManager>,
) -> Result<(), errors::AppError> {
    system_audio::stop(&manager).map_err(errors::classify)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            tooling::initialize(app.path().resource_dir()?);
            // Captures and extracted media remain available to the in-app player.
            let recordings = app.path().app_data_dir()?.join("recordings");
            std::fs::create_dir_all(&recordings)?;
            app.asset_protocol_scope()
                .allow_directory(recordings, true)?;
            Ok(())
        })
        .manage(system_audio::SystemAudioManager::default())
        .manage(realtime::RealtimeManager::default())
        .manage(online_media::OnlineMediaManager::default())
        .manage(transcription::TranscriptionManager::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            inspect_audio,
            allow_media_preview,
            append_technical_log,
            inspect_online_media,
            download_online_caption,
            inspect_web_page,
            extract_online_media,
            cancel_online_media,
            list_api_keys,
            list_available_models,
            list_audio_input_devices,
            save_api_key,
            select_api_key,
            delete_api_key,
            write_transcript,
            append_saved_transcript,
            replace_saved_transcript_chunk,
            save_transcript_summary,
            save_transcript_notes,
            fetch_project_costs,
            list_admin_api_keys,
            save_admin_api_key,
            get_minefia_projects,
            list_openai_projects,
            minefia_usage_ready,
            create_minefia_project_key,
            select_admin_api_key,
            delete_admin_api_key,
            save_transcript_translation,
            save_summary_translation,
            save_notes_translation,
            snapshot_saved_transcript,
            replace_final_transcript,
            save_enhanced_transcript,
            cleanup_saved_transcript_chunks,
            list_saved_transcripts,
            list_library_files,
            delete_library_file,
            reveal_library_path,
            import_local_media,
            extract_local_audio,
            delete_saved_transcript,
            translate_transcript,
            update_summary_notes,
            generate_final_notes,
            summarize_transcript,
            ask_about_transcript,
            enhance_transcript,
            transcribe_audio,
            list_resumable_transcriptions,
            resume_transcription,
            cancel_transcription,
            start_system_audio,
            stop_system_audio,
            start_realtime_audio,
            stop_realtime_audio
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Minefia Audio");
}
