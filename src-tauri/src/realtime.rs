use crate::system_audio::{self, PcmBatch};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};
use tauri::Emitter;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::header::AUTHORIZATION, Message},
};

const REALTIME_MODEL: &str = "gpt-live-transcribe";
const LIVE_PRICE_PER_HOUR: f64 = crate::pricing::LIVE_TRANSCRIPTION_PER_MINUTE * 60.0;
const LIVE_SAMPLE_RATE: u32 = 24_000;
const VAD_SILENCE_SECONDS: f64 = 0.6;
const VAD_THRESHOLD: i16 = 184; // Client-side end-of-turn detection; independent from Standard Mode's -35 dBFS splitting.

#[derive(Default)]
pub struct RealtimeManager {
    active: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RealtimeEvent {
    kind: &'static str,
    elapsed_seconds: f64,
    estimated_cost: f64,
    transcript: Option<String>,
    partial: bool,
    message: Option<String>,
    code: Option<String>,
}

pub fn start(
    app: tauri::AppHandle,
    manager: &RealtimeManager,
    api_key: String,
    source: String,
    input_device: Option<String>,
) -> Result<(), String> {
    system_audio::validate_source(&source, input_device.as_deref())?;
    let mut active = manager
        .active
        .lock()
        .map_err(|_| "Recording state is unavailable.")?;
    if active
        .as_ref()
        .is_some_and(|stopped| !stopped.load(Ordering::Acquire))
    {
        return Err("A live transcription session is already running.".into());
    }
    if active.is_some() {
        *active = None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    *active = Some(stop.clone());
    drop(active);
    tauri::async_runtime::spawn(run(app, api_key, source, input_device, stop));
    Ok(())
}

pub fn stop(manager: &RealtimeManager) -> Result<(), String> {
    let active = manager
        .active
        .lock()
        .map_err(|_| "Recording state is unavailable.")?;
    let Some(stop) = active.as_ref() else {
        return Err("No live transcription session is running.".into());
    };
    stop.store(true, Ordering::Release);
    Ok(())
}

async fn run(
    app: tauri::AppHandle,
    api_key: String,
    source: String,
    input_device: Option<String>,
    stop: Arc<AtomicBool>,
) {
    let (audio_tx, audio_rx) = mpsc::sync_channel::<PcmBatch>(2048);
    let capture_stop = stop.clone();
    let capture_app = app.clone();
    let capture = std::thread::Builder::new()
        .name("minefia-live-audio-capture".into())
        .spawn(move || {
            let capture_control = capture_stop.clone();
            if let Err(message) = system_audio::capture_source(
                capture_stop,
                audio_tx,
                &source,
                input_device.as_deref(),
                &capture_app,
            ) {
                if !capture_control.load(Ordering::Acquire) {
                    emit(&capture_app, "error", 0.0, None, false, Some(message));
                    capture_control.store(true, Ordering::Release);
                }
            }
        });
    let capture = match capture {
        Ok(capture) => capture,
        Err(error) => {
            emit(&app, "error", 0.0, None, false, Some(error.to_string()));
            stop.store(true, Ordering::Release);
            return;
        }
    };

    let start = tokio::time::Instant::now();
    let mut retry = 0u32;
    let mut quiet_samples = 0usize;
    let mut speech_samples = 0usize;
    let mut last_progress = tokio::time::Instant::now();
    let mut captions = HashMap::<String, String>::new();
    let mut item_order = Vec::<String>::new();

    while !stop.load(Ordering::Acquire) {
        let mut request = match format!("wss://api.openai.com/v1/realtime?model={REALTIME_MODEL}")
            .into_client_request()
        {
            Ok(request) => request,
            Err(error) => {
                emit(
                    &app,
                    "error",
                    start.elapsed().as_secs_f64(),
                    None,
                    false,
                    Some(error.to_string()),
                );
                break;
            }
        };
        let authorization = match format!("Bearer {api_key}")
            .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        {
            Ok(header) => header,
            Err(error) => {
                emit(
                    &app,
                    "error",
                    start.elapsed().as_secs_f64(),
                    None,
                    false,
                    Some(error.to_string()),
                );
                break;
            }
        };
        request.headers_mut().insert(AUTHORIZATION, authorization);
        let connection =
            tokio::time::timeout(Duration::from_secs(20), connect_async(request)).await;
        let Ok(Ok((socket, _))) = connection else {
            if stop.load(Ordering::Acquire) {
                break;
            }
            retry += 1;
            if retry > 5 {
                emit(&app, "error", start.elapsed().as_secs_f64(), None, false, Some("Realtime connection failed after several retries. Check the network and API access, then start again.".into()));
                break;
            }
            emit(
                &app,
                "reconnecting",
                start.elapsed().as_secs_f64(),
                None,
                false,
                Some(format!("Connection interrupted. Reconnecting ({retry}/5)…")),
            );
            tokio::time::sleep(Duration::from_secs(1 << (retry - 1))).await;
            continue;
        };
        let (mut write, mut read) = socket.split();
        let configuration = json!({
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": {"input": {
                    "format": {"type": "audio/pcm", "rate": LIVE_SAMPLE_RATE},
                    "transcription": {"model": REALTIME_MODEL, "delay": "low"},
                    "turn_detection": null
                }}
            }
        });
        let mut disconnected = write
            .send(Message::Text(configuration.to_string().into()))
            .await
            .is_err();
        let mut configured = false;
        let config_started = tokio::time::Instant::now();

        while !stop.load(Ordering::Acquire) && !disconnected {
            if configured {
                if let Ok(batch) = audio_rx.try_recv() {
                    let samples = stereo_48k_to_mono_24k(&batch.samples);
                    if !samples.is_empty() {
                        let max_level = samples
                            .iter()
                            .map(|value| value.unsigned_abs())
                            .max()
                            .unwrap_or(0);
                        system_audio::emit_input_level(&app, max_level as f32 / i16::MAX as f32);
                        let encoded = encode_pcm(&samples);
                        if write
                            .send(Message::Text(
                                json!({"type":"input_audio_buffer.append","audio":encoded})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            disconnected = true;
                        }
                        if max_level > VAD_THRESHOLD as u16 {
                            speech_samples += samples.len();
                            quiet_samples = 0;
                        } else {
                            quiet_samples += samples.len();
                        }
                        if configured
                            && speech_samples > 0
                            && quiet_samples
                                >= (LIVE_SAMPLE_RATE as f64 * VAD_SILENCE_SECONDS) as usize
                        {
                            if write
                                .send(Message::Text(
                                    json!({"type":"input_audio_buffer.commit"})
                                        .to_string()
                                        .into(),
                                ))
                                .await
                                .is_err()
                            {
                                disconnected = true;
                            }
                            speech_samples = 0;
                            quiet_samples = 0;
                        }
                    }
                }
            }

            tokio::select! {
                message = read.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(event) = serde_json::from_str::<Value>(&text) {
                            let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
                            if kind == "session.updated" { configured = true; retry = 0; }
                            if kind == "error" {
                                let error = event.pointer("/error/message").and_then(Value::as_str).unwrap_or("Realtime API rejected the session.").to_owned();
                                emit(&app, "error", start.elapsed().as_secs_f64(), None, false, Some(error));
                                stop.store(true, Ordering::Release);
                                disconnected = true;
                            }
                            update_caption(&event, &mut captions, &mut item_order);
                            if kind == "conversation.item.input_audio_transcription.delta" || kind == "conversation.item.input_audio_transcription.completed" {
                                let transcript = item_order.iter().filter_map(|id| captions.get(id)).filter(|text| !text.is_empty()).cloned().collect::<Vec<_>>().join("\n");
                                emit(&app, "transcript", start.elapsed().as_secs_f64(), Some(transcript), kind.ends_with(".delta"), None);
                            }
                        }
                    },
                    Some(Ok(Message::Ping(data))) => { let _ = write.send(Message::Pong(data)).await; },
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => disconnected = true,
                    _ => {}
                },
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            if !configured && config_started.elapsed() >= Duration::from_secs(15) {
                disconnected = true;
            }
            if last_progress.elapsed() >= Duration::from_millis(500) {
                let elapsed = start.elapsed().as_secs_f64();
                emit(&app, "status", elapsed, None, false, None);
                last_progress = tokio::time::Instant::now();
            }
        }
        if stop.load(Ordering::Acquire) {
            if speech_samples > 0 {
                let _ = write
                    .send(Message::Text(
                        json!({"type":"input_audio_buffer.commit"})
                            .to_string()
                            .into(),
                    ))
                    .await;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let message = tokio::time::timeout_at(deadline, read.next()).await;
                    let Ok(Some(Ok(Message::Text(text)))) = message else {
                        break;
                    };
                    let Ok(event) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    let kind = event
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    update_caption(&event, &mut captions, &mut item_order);
                    if kind == "conversation.item.input_audio_transcription.delta"
                        || kind == "conversation.item.input_audio_transcription.completed"
                    {
                        let transcript = item_order
                            .iter()
                            .filter_map(|id| captions.get(id))
                            .filter(|text| !text.is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        emit(
                            &app,
                            "transcript",
                            start.elapsed().as_secs_f64(),
                            Some(transcript),
                            kind.ends_with(".delta"),
                            None,
                        );
                    }
                    if kind == "conversation.item.input_audio_transcription.completed" {
                        break;
                    }
                }
            }
            let _ = write.send(Message::Close(None)).await;
            break;
        }
        if disconnected && !stop.load(Ordering::Acquire) {
            retry += 1;
            if retry > 5 {
                emit(&app, "error", start.elapsed().as_secs_f64(), None, false, Some("Realtime connection could not be restored. This live transcript is incomplete; check the network and restart the session.".into()));
                break;
            }
            emit(
                &app,
                "reconnecting",
                start.elapsed().as_secs_f64(),
                None,
                false,
                Some(format!("Connection interrupted. Reconnecting ({retry}/5)…")),
            );
            tokio::time::sleep(Duration::from_secs(1 << (retry - 1))).await;
        }
    }

    stop.store(true, Ordering::Release);
    let _ = capture.join();
    let _ = app.emit(
        "realtime-audio-progress",
        RealtimeEvent {
            kind: "done",
            elapsed_seconds: start.elapsed().as_secs_f64(),
            estimated_cost: start.elapsed().as_secs_f64() / 3600.0 * LIVE_PRICE_PER_HOUR,
            transcript: None,
            partial: false,
            message: None,
            code: None,
        },
    );
}

fn update_caption(event: &Value, captions: &mut HashMap<String, String>, order: &mut Vec<String>) {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let item_id = event.get("item_id").and_then(Value::as_str);
    if kind == "input_audio_buffer.committed" {
        if let Some(id) = event.get("item_id").and_then(Value::as_str) {
            let previous = event.get("previous_item_id").and_then(Value::as_str);
            if !order.contains(&id.to_owned()) {
                if let Some(previous) =
                    previous.and_then(|value| order.iter().position(|item| item == value))
                {
                    order.insert(previous + 1, id.to_owned());
                } else {
                    order.push(id.to_owned());
                }
            }
        }
    }
    let Some(id) = item_id else {
        return;
    };
    if !order.contains(&id.to_owned()) {
        order.push(id.to_owned());
    }
    match kind {
        "conversation.item.input_audio_transcription.delta" => {
            captions.entry(id.to_owned()).or_default().push_str(
                event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }
        "conversation.item.input_audio_transcription.completed" => {
            captions.insert(
                id.to_owned(),
                event
                    .get("transcript")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        _ => {}
    }
}

fn stereo_48k_to_mono_24k(input: &[i16]) -> Vec<i16> {
    input
        .chunks_exact(4)
        .map(|frame| {
            ((frame[0] as i32 + frame[1] as i32 + frame[2] as i32 + frame[3] as i32) / 4) as i16
        })
        .collect()
}

fn encode_pcm(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    BASE64.encode(bytes)
}

fn emit(
    app: &tauri::AppHandle,
    kind: &'static str,
    elapsed: f64,
    transcript: Option<String>,
    partial: bool,
    message: Option<String>,
) {
    let code = (kind == "error").then(|| {
        crate::errors::classify(message.as_deref().unwrap_or("Realtime API request failed."))
            .code
            .to_owned()
    });
    let _ = app.emit(
        "realtime-audio-progress",
        RealtimeEvent {
            kind,
            elapsed_seconds: elapsed,
            estimated_cost: elapsed / 3600.0 * LIVE_PRICE_PER_HOUR,
            transcript,
            partial,
            message,
            code,
        },
    );
}
