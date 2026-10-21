use crate::{audio, transcription};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
};
use tauri::Emitter;
use tokio::sync::{mpsc as tokio_mpsc, Semaphore};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const TARGET_SECONDS: u64 = 90;
const FORCE_SECONDS: u64 = 150;
const SILENCE_SECONDS: f64 = 0.5;
const SILENCE_AMPLITUDE: i16 = 583; // 20 * log10(583 / 32767) ~= -35 dBFS
const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MIX_BLOCK_SAMPLES: usize = 960; // 10 ms at 48 kHz stereo.
const MAX_MIX_BUFFER_SAMPLES: usize = SAMPLE_RATE as usize * CHANNELS as usize * 5;

#[derive(Default)]
pub struct SystemAudioManager {
    active: Mutex<Option<SessionControl>>,
}

struct SessionControl {
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(crate) struct PcmBatch {
    pub(crate) samples: Vec<i16>,
}

struct AudioChunk {
    index: usize,
    samples: Vec<i16>,
    duration_seconds: f64,
    forced: bool,
}

struct ChunkResult {
    index: usize,
    text: Result<String, String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemAudioEvent {
    kind: &'static str,
    elapsed_seconds: f64,
    chunk_seconds: f64,
    completed_chunks: usize,
    queued_chunks: usize,
    active_requests: usize,
    waiting_for_silence: bool,
    force_split_in_seconds: f64,
    estimated_cost: f64,
    chunk: Option<usize>,
    text: Option<String>,
    message: Option<String>,
    code: Option<String>,
    input_level: Option<f32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevice {
    pub id: String,
    pub name: String,
}

pub(crate) fn emit_input_level(app: &tauri::AppHandle, level: f32) {
    static LAST_EMIT: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
    let last = LAST_EMIT.get_or_init(|| Mutex::new(std::time::Instant::now()));
    if let Ok(mut last) = last.lock() {
        if last.elapsed() >= std::time::Duration::from_millis(100) {
            *last = std::time::Instant::now();
            let _ = app.emit("audio-input-level", level.clamp(0.0, 1.0));
        }
    }
}

pub fn list_input_devices() -> Result<Vec<InputDevice>, String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|error| error.to_string())?;
    Ok(devices
        .enumerate()
        .filter_map(|(index, device)| {
            device.name().ok().map(|name| InputDevice {
                id: index.to_string(),
                name,
            })
        })
        .collect())
}

pub fn start(
    app: tauri::AppHandle,
    manager: &SystemAudioManager,
    api_key: String,
    model: String,
    source: String,
    input_device: Option<String>,
    parallel_requests: usize,
) -> Result<(), String> {
    validate_source(&source, input_device.as_deref())?;
    let price_per_minute = crate::pricing::standard_transcription_per_minute(&model)
        .ok_or("This transcription model is not supported by Minefia Audio.")?;
    let parallel_requests = parallel_requests.clamp(1, 4);
    let mut active = manager
        .active
        .lock()
        .map_err(|_| "Recording state is unavailable.")?;
    if active
        .as_ref()
        .is_some_and(|session| session.running.load(Ordering::Acquire))
    {
        return Err("A system-audio recording is already running.".into());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    *active = Some(SessionControl {
        stop: stop.clone(),
        running: running.clone(),
    });
    drop(active);

    let (batch_tx, batch_rx) = mpsc::sync_channel::<PcmBatch>(256);
    let (chunk_tx, chunk_rx) = tokio_mpsc::channel::<AudioChunk>(4);
    let processor_app = app.clone();
    std::thread::Builder::new()
        .name("minefia-standard-chunker".into())
        .spawn(move || process_audio(batch_rx, chunk_tx, processor_app, price_per_minute))
        .map_err(|error| error.to_string())?;

    let capture_app = app.clone();
    let capture_stop = stop.clone();
    std::thread::Builder::new()
        .name("minefia-system-audio".into())
        .spawn(move || {
            if let Err(message) = capture_source(
                capture_stop,
                batch_tx,
                &source,
                input_device.as_deref(),
                &capture_app,
            ) {
                emit_error(&capture_app, message);
            }
        })
        .map_err(|error| error.to_string())?;

    tauri::async_runtime::spawn(run_transcription_queue(
        app,
        chunk_rx,
        api_key,
        model,
        running,
        parallel_requests,
    ));
    Ok(())
}

pub(crate) fn capture_source(
    stop: Arc<AtomicBool>,
    sender: mpsc::SyncSender<PcmBatch>,
    source: &str,
    input_device: Option<&str>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let recordings = crate::storage::dated_recording_dir(app)?;
    std::fs::create_dir_all(&recordings).map_err(|error| error.to_string())?;
    if source == "both" {
        return capture_both_sources(stop, sender, input_device, app, recordings);
    }
    let (capture_tx, capture_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let capture_stop = stop.clone();
    let capture_app = app.clone();
    let source = source.to_owned();
    let input_device = input_device.map(str::to_owned);
    let worker = std::thread::Builder::new()
        .name("minefia-audio-source".into())
        .spawn(move || {
            if source == "microphone" {
                capture_microphone(
                    capture_stop,
                    capture_tx,
                    input_device.as_deref(),
                    &capture_app,
                )
            } else {
                platform::capture(capture_stop, capture_tx)
            }
        })
        .map_err(|error| error.to_string())?;

    let mut recorder = CaptureWaveRecorder::new(recordings)?;
    let mut receiver_error = None;
    while let Ok(batch) = capture_rx.recv() {
        if let Err(error) = recorder.write(&batch.samples) {
            receiver_error = Some(error);
            stop.store(true, Ordering::Release);
            break;
        }
        if sender.send(batch).is_err() {
            stop.store(true, Ordering::Release);
            break;
        }
    }
    let capture_result = worker
        .join()
        .map_err(|_| "Audio capture worker stopped unexpectedly.".to_owned())?;
    let saved = recorder.finish()?;
    if !saved.is_empty() {
        let _ = app.emit("audio-recordings-saved", &saved);
    }
    if let Some(error) = receiver_error {
        return Err(error);
    }
    capture_result
}

// Persist generated capture PCM in the app-data directory while it is streamed onward.
// Rotate before the RIFF/WAV 32-bit size limit rather than allowing an invalid large file.
struct CaptureWaveRecorder {
    directory: std::path::PathBuf,
    file: Option<File>,
    path: Option<std::path::PathBuf>,
    bytes: u64,
    saved: Vec<String>,
}

impl CaptureWaveRecorder {
    const MAX_DATA_BYTES: u64 = 1_000_000_000;

    fn new(directory: std::path::PathBuf) -> Result<Self, String> {
        Ok(Self {
            directory,
            file: None,
            path: None,
            bytes: 0,
            saved: Vec::new(),
        })
    }

    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let mut offset = 0;
        while offset < samples.len() {
            if self.file.is_none() || self.bytes >= Self::MAX_DATA_BYTES {
                self.open_next()?;
            }
            let available = ((Self::MAX_DATA_BYTES - self.bytes) / 2) as usize;
            let end = (offset + available).min(samples.len());
            let mut bytes = Vec::with_capacity((end - offset) * 2);
            for sample in &samples[offset..end] {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            self.file
                .as_mut()
                .expect("opened above")
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            self.bytes += bytes.len() as u64;
            offset = end;
            if self.bytes >= Self::MAX_DATA_BYTES {
                self.close_current()?;
            }
        }
        Ok(())
    }

    fn open_next(&mut self) -> Result<(), String> {
        self.close_current()?;
        let timestamp = crate::storage::timestamp_millis();
        let path = self.directory.join(format!("rec_{timestamp}.wav"));
        let mut file = File::create(&path).map_err(|error| error.to_string())?;
        file.write_all(&[0; 44])
            .map_err(|error| error.to_string())?;
        self.file = Some(file);
        self.path = Some(path);
        self.bytes = 0;
        Ok(())
    }

    fn close_current(&mut self) -> Result<(), String> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        if self.bytes == 0 {
            let _ = std::fs::remove_file(path);
            return Ok(());
        }
        let riff_size = u32::try_from(36 + self.bytes)
            .map_err(|_| "Capture segment exceeded WAV size limit.".to_owned())?;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(b"RIFF"))
            .and_then(|_| file.write_all(&riff_size.to_le_bytes()))
            .and_then(|_| file.write_all(b"WAVEfmt "))
            .and_then(|_| file.write_all(&16_u32.to_le_bytes()))
            .and_then(|_| file.write_all(&1_u16.to_le_bytes()))
            .and_then(|_| file.write_all(&CHANNELS.to_le_bytes()))
            .and_then(|_| file.write_all(&SAMPLE_RATE.to_le_bytes()))
            .and_then(|_| file.write_all(&(SAMPLE_RATE * CHANNELS as u32 * 2).to_le_bytes()))
            .and_then(|_| file.write_all(&(CHANNELS * 2).to_le_bytes()))
            .and_then(|_| file.write_all(&16_u16.to_le_bytes()))
            .and_then(|_| file.write_all(b"data"))
            .and_then(|_| file.write_all(&(self.bytes as u32).to_le_bytes()))
            .map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        self.saved.push(path.to_string_lossy().into_owned());
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<String>, String> {
        self.close_current()?;
        Ok(self.saved)
    }
}

pub(crate) fn validate_source(source: &str, input_device: Option<&str>) -> Result<(), String> {
    match source {
        "system" => Ok(()),
        "microphone" if input_device.is_some() => Ok(()),
        "both" if input_device.is_some() => Ok(()),
        "both" => Err("Select an available microphone before starting capture.".into()),
        "microphone" => Err("Select an available microphone before starting capture.".into()),
        _ => Err("Choose either System Audio or a microphone as the input source.".into()),
    }
}

fn capture_both_sources(
    stop: Arc<AtomicBool>,
    sender: mpsc::SyncSender<PcmBatch>,
    input_device: Option<&str>,
    app: &tauri::AppHandle,
    recordings: std::path::PathBuf,
) -> Result<(), String> {
    let mut recorder = CaptureWaveRecorder::new(recordings)?;
    let (system_tx, system_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let (microphone_tx, microphone_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let system_stop = stop.clone();
    let system_worker = std::thread::Builder::new()
        .name("minefia-system-audio-source".into())
        .spawn(move || platform::capture(system_stop, system_tx))
        .map_err(|error| error.to_string())?;
    let microphone_stop = stop.clone();
    let microphone_app = app.clone();
    let input_device = input_device.map(str::to_owned);
    let microphone_worker = match std::thread::Builder::new()
        .name("minefia-microphone-source".into())
        .spawn(move || {
            capture_microphone(
                microphone_stop,
                microphone_tx,
                input_device.as_deref(),
                &microphone_app,
            )
        }) {
        Ok(worker) => worker,
        Err(error) => {
            stop.store(true, Ordering::Release);
            let _ = system_worker.join();
            return Err(error.to_string());
        }
    };
    let mut system_samples = std::collections::VecDeque::new();
    let mut microphone_samples = std::collections::VecDeque::new();
    let mut system_closed = false;
    let mut microphone_closed = false;
    let mut failure = None;

    loop {
        drain_capture_batches(&system_rx, &mut system_samples, &mut system_closed);
        drain_capture_batches(
            &microphone_rx,
            &mut microphone_samples,
            &mut microphone_closed,
        );
        if (system_closed || microphone_closed) && !stop.load(Ordering::Acquire) {
            failure = Some(if system_closed {
                "System audio capture stopped while the combined recording was active.".to_owned()
            } else {
                "Microphone disconnected while the combined recording was active.".to_owned()
            });
            stop.store(true, Ordering::Release);
        }
        if !stop.load(Ordering::Acquire)
            && (system_samples.len() > MAX_MIX_BUFFER_SAMPLES
                || microphone_samples.len() > MAX_MIX_BUFFER_SAMPLES)
        {
            failure = Some("Combined audio input exceeded its safe synchronization buffer.".into());
            stop.store(true, Ordering::Release);
        }
        while system_samples.len() >= MIX_BLOCK_SAMPLES
            && microphone_samples.len() >= MIX_BLOCK_SAMPLES
        {
            let mixed = mix_audio_block(&mut system_samples, &mut microphone_samples);
            if let Err(error) = recorder.write(&mixed) {
                failure = Some(error);
                stop.store(true, Ordering::Release);
                break;
            }
            if sender.send(PcmBatch { samples: mixed }).is_err() {
                failure = Some("The audio processing queue closed during combined capture.".into());
                stop.store(true, Ordering::Release);
                break;
            }
        }
        if stop.load(Ordering::Acquire) && system_closed && microphone_closed {
            while !system_samples.is_empty() || !microphone_samples.is_empty() {
                let mixed = mix_audio_block(&mut system_samples, &mut microphone_samples);
                if let Err(error) = recorder.write(&mixed) {
                    failure = Some(error);
                    break;
                }
                if sender.send(PcmBatch { samples: mixed }).is_err() {
                    failure =
                        Some("The audio processing queue closed during combined capture.".into());
                    break;
                }
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    match system_worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(message)) if failure.is_none() => failure = Some(message),
        Err(_) if failure.is_none() => {
            failure = Some("System audio capture stopped unexpectedly.".into())
        }
        _ => {}
    }
    match microphone_worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(message)) if failure.is_none() => failure = Some(message),
        Err(_) if failure.is_none() => {
            failure = Some("Microphone capture stopped unexpectedly.".into())
        }
        _ => {}
    }
    let saved = recorder.finish()?;
    if !saved.is_empty() {
        let _ = app.emit("audio-recordings-saved", &saved);
    }
    if let Some(message) = failure {
        Err(message)
    } else {
        Ok(())
    }
}

fn drain_capture_batches(
    receiver: &mpsc::Receiver<PcmBatch>,
    destination: &mut std::collections::VecDeque<i16>,
    closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(batch) => destination.extend(batch.samples),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                *closed = true;
                break;
            }
        }
    }
}

fn mix_audio_block(
    system: &mut std::collections::VecDeque<i16>,
    microphone: &mut std::collections::VecDeque<i16>,
) -> Vec<i16> {
    (0..MIX_BLOCK_SAMPLES)
        .map(|_| {
            let system = system.pop_front().unwrap_or_default() as i32;
            let microphone = microphone.pop_front().unwrap_or_default() as i32;
            ((system + microphone) / 2) as i16
        })
        .collect()
}

fn capture_microphone(
    stop: Arc<AtomicBool>,
    sender: mpsc::SyncSender<PcmBatch>,
    input_device: Option<&str>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|error| error.to_string())?;
    let selected_index = input_device
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let device = devices
        .enumerate()
        .find_map(|(index, device)| (index == selected_index).then_some(device))
        .ok_or("The selected microphone is no longer connected.")?;
    let supported = device
        .default_input_config()
        .map_err(|error| format!("Could not open microphone: {error}"))?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    if channels == 0 || sample_rate == 0 {
        return Err("The microphone reported an invalid audio format.".into());
    }
    let config: cpal::StreamConfig = supported.clone().into();
    let stream_error = Arc::new(Mutex::new(None::<String>));
    let callback_error = stream_error.clone();
    let callback_sender = sender;
    let phase = Arc::new(Mutex::new(0_u64));
    let callback_phase = phase.clone();
    let stream_error_callback = move |error| {
        if let Ok(mut stored) = callback_error.lock() {
            *stored = Some(format!("Microphone disconnected or failed: {error}"));
        }
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => {
            let callback_app = app.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    send_microphone_samples(
                        data.to_vec(),
                        sample_rate,
                        channels,
                        &callback_phase,
                        &callback_sender,
                        &callback_app,
                    )
                },
                stream_error_callback,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let callback_phase = phase.clone();
            let callback_sender = callback_sender.clone();
            let app = app.clone();
            let stream_error = stream_error.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    send_microphone_samples(
                        data.iter()
                            .map(|sample| *sample as f32 / i16::MAX as f32)
                            .collect(),
                        sample_rate,
                        channels,
                        &callback_phase,
                        &callback_sender,
                        &app,
                    )
                },
                move |error| {
                    if let Ok(mut stored) = stream_error.lock() {
                        *stored = Some(format!("Microphone disconnected or failed: {error}"));
                    }
                },
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let callback_phase = phase;
            let callback_sender = callback_sender.clone();
            let app = app.clone();
            let stream_error = stream_error.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    send_microphone_samples(
                        data.iter()
                            .map(|sample| (*sample as f32 - 32768.0) / 32768.0)
                            .collect(),
                        sample_rate,
                        channels,
                        &callback_phase,
                        &callback_sender,
                        &app,
                    )
                },
                move |error| {
                    if let Ok(mut stored) = stream_error.lock() {
                        *stored = Some(format!("Microphone disconnected or failed: {error}"));
                    }
                },
                None,
            )
        }
        format => return Err(format!("Microphone format {format:?} is not supported.")),
    }
    .map_err(|error| format!("Could not start microphone: {error}"))?;
    stream
        .play()
        .map_err(|error| format!("Microphone permission or device error: {error}"))?;
    while !stop.load(Ordering::Acquire) {
        if let Ok(mut stored) = stream_error.lock() {
            if let Some(error) = stored.take() {
                return Err(error);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(stream);
    Ok(())
}

fn send_microphone_samples(
    input: Vec<f32>,
    source_rate: u32,
    source_channels: usize,
    phase: &Mutex<u64>,
    sender: &mpsc::SyncSender<PcmBatch>,
    app: &tauri::AppHandle,
) {
    let Ok(mut phase) = phase.lock() else {
        return;
    };
    let mut output = Vec::new();
    let mut level_sum = 0.0_f64;
    let mut source_frames = 0usize;
    for frame in input.chunks_exact(source_channels) {
        let left = frame[0].clamp(-1.0, 1.0);
        let right = if source_channels > 1 {
            frame[1].clamp(-1.0, 1.0)
        } else {
            left
        };
        level_sum += ((left as f64).powi(2) + (right as f64).powi(2)) / 2.0;
        source_frames += 1;
        *phase += SAMPLE_RATE as u64;
        while *phase >= source_rate as u64 {
            output.push((left * i16::MAX as f32).round() as i16);
            output.push((right * i16::MAX as f32).round() as i16);
            *phase -= source_rate as u64;
        }
    }
    if source_frames > 0 {
        let level = (level_sum / source_frames as f64).sqrt() as f32;
        emit_input_level(app, level);
    }
    if !output.is_empty() {
        let _ = sender.send(PcmBatch { samples: output });
    }
}

pub fn stop(manager: &SystemAudioManager) -> Result<(), String> {
    let active = manager
        .active
        .lock()
        .map_err(|_| "Recording state is unavailable.")?;
    let Some(session) = active.as_ref() else {
        return Err("No system-audio recording is running.".into());
    };
    session.stop.store(true, Ordering::Release);
    Ok(())
}

fn process_audio(
    batches: mpsc::Receiver<PcmBatch>,
    chunks: tokio_mpsc::Sender<AudioChunk>,
    app: tauri::AppHandle,
    price_per_minute: f64,
) {
    let mut chunker = SilenceChunker::default();
    while let Ok(batch) = batches.recv() {
        let level = if batch.samples.is_empty() {
            0.0
        } else {
            (batch
                .samples
                .iter()
                .map(|sample| (*sample as f64).powi(2))
                .sum::<f64>()
                / batch.samples.len() as f64)
                .sqrt() as f32
                / i16::MAX as f32
        };
        emit_input_level(&app, level);
        for chunk in chunker.push(batch.samples) {
            if chunk.forced {
                let _ = app.emit("system-audio-progress", SystemAudioEvent {
                    kind: "warning",
                    elapsed_seconds: 0.0,
                    chunk_seconds: chunk.duration_seconds,
                    completed_chunks: 0,
                    queued_chunks: chunk.index,
                    active_requests: 0,
                    waiting_for_silence: false,
                    force_split_in_seconds: 0.0,
                    estimated_cost: 0.0,
                    chunk: Some(chunk.index),
                    text: None,
                    message: Some("No qualifying silence was found before the safety limit, so this chunk was force-split.".into()),
                    code: None,
                    input_level: None,
                });
            }
            if chunks.blocking_send(chunk).is_err() {
                return;
            }
        }
        chunker.emit_status(&app, price_per_minute);
    }
    if let Some(chunk) = chunker.finish() {
        let _ = chunks.blocking_send(chunk);
    }
}

#[derive(Default)]
struct SilenceChunker {
    buffer: Vec<i16>,
    silent_frames: usize,
    total_frames: u64,
    next_chunk_index: usize,
    last_status_frame: u64,
}

impl SilenceChunker {
    fn push(&mut self, samples: Vec<i16>) -> Vec<AudioChunk> {
        let mut completed = Vec::new();
        for frame in samples.chunks_exact(CHANNELS as usize) {
            self.buffer.extend_from_slice(frame);
            self.total_frames += 1;
            if frame
                .iter()
                .all(|sample| sample.unsigned_abs() <= SILENCE_AMPLITUDE as u16)
            {
                self.silent_frames += 1;
            } else {
                self.silent_frames = 0;
            }

            let current_frames = self.buffer.len() / CHANNELS as usize;
            let target_frames = TARGET_SECONDS as usize * SAMPLE_RATE as usize;
            let force_frames = FORCE_SECONDS as usize * SAMPLE_RATE as usize;
            let silence_frames = (SILENCE_SECONDS * SAMPLE_RATE as f64) as usize;
            let memory_limit = self.buffer.len() * size_of::<i16>() >= MAX_MEMORY_BYTES;

            if current_frames >= target_frames && self.silent_frames >= silence_frames {
                let silence_midpoint = current_frames - self.silent_frames / 2;
                let boundary = silence_midpoint.max(target_frames);
                completed.push(self.take_chunk(boundary, false));
            } else if current_frames >= force_frames || memory_limit {
                completed.push(self.take_chunk(current_frames, true));
            }
        }
        completed
    }

    fn take_chunk(&mut self, frames: usize, forced: bool) -> AudioChunk {
        let split = frames * CHANNELS as usize;
        let remainder = self.buffer.split_off(split);
        let samples = std::mem::replace(&mut self.buffer, remainder);
        self.silent_frames = trailing_silent_frames(&self.buffer);
        self.next_chunk_index += 1;
        AudioChunk {
            index: self.next_chunk_index,
            duration_seconds: frames as f64 / SAMPLE_RATE as f64,
            samples,
            forced,
        }
    }

    fn finish(&mut self) -> Option<AudioChunk> {
        if self.buffer.is_empty() {
            None
        } else {
            let frames = self.buffer.len() / CHANNELS as usize;
            Some(self.take_chunk(frames, false))
        }
    }

    fn emit_status(&mut self, app: &tauri::AppHandle, price_per_minute: f64) {
        if self.total_frames.saturating_sub(self.last_status_frame) < SAMPLE_RATE as u64 / 4 {
            return;
        }
        self.last_status_frame = self.total_frames;
        let elapsed = self.total_frames as f64 / SAMPLE_RATE as f64;
        let current = self.buffer.len() as f64 / CHANNELS as f64 / SAMPLE_RATE as f64;
        let waiting = current >= TARGET_SECONDS as f64;
        let _ = app.emit(
            "system-audio-progress",
            SystemAudioEvent {
                kind: "status",
                elapsed_seconds: elapsed,
                chunk_seconds: current,
                completed_chunks: 0,
                queued_chunks: self.next_chunk_index,
                active_requests: 0,
                waiting_for_silence: waiting,
                force_split_in_seconds: (FORCE_SECONDS as f64 - current).max(0.0),
                estimated_cost: elapsed / 60.0 * price_per_minute,
                chunk: None,
                text: None,
                message: None,
                code: None,
                input_level: None,
            },
        );
    }
}

fn trailing_silent_frames(samples: &[i16]) -> usize {
    samples
        .chunks_exact(CHANNELS as usize)
        .rev()
        .take_while(|frame| {
            frame
                .iter()
                .all(|sample| sample.unsigned_abs() <= SILENCE_AMPLITUDE as u16)
        })
        .count()
}

async fn run_transcription_queue(
    app: tauri::AppHandle,
    mut chunks: tokio_mpsc::Receiver<AudioChunk>,
    api_key: String,
    model: String,
    running: Arc<AtomicBool>,
    parallel_requests: usize,
) {
    let temp = match tempfile::tempdir() {
        Ok(value) => Arc::new(value),
        Err(error) => {
            emit_error(&app, error.to_string());
            running.store(false, Ordering::Release);
            return;
        }
    };
    let semaphore = Arc::new(Semaphore::new(parallel_requests.clamp(1, 4)));
    let mut queue_tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let (result_tx, mut result_rx) = tokio_mpsc::unbounded_channel::<ChunkResult>();
    let mut capture_closed = false;
    let mut pending = 0usize;
    let mut next_result = 1usize;
    let mut ordered = BTreeMap::<usize, Option<String>>::new();

    while !capture_closed || pending > 0 {
        tokio::select! {
            chunk = chunks.recv(), if !capture_closed => {
                match chunk {
                    Some(chunk) => {
                        pending += 1;
                        let permit = semaphore.clone();
                        let result_tx = result_tx.clone();
                        let api_key = api_key.clone();
                        let model = model.clone();
                        let temp = temp.clone();
                        let request_app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _permit = permit.acquire_owned().await;
                            let index = chunk.index;
                            let text = transcribe_chunk(&request_app, &temp, chunk, &api_key, &model)
                                .await
                                .map_err(|error| error.to_string());
                            let _ = result_tx.send(ChunkResult { index, text });
                        });
                    }
                    None => capture_closed = true,
                }
            }
            result = result_rx.recv(), if pending > 0 => {
                let Some(result) = result else { break; };
                pending -= 1;
                match result.text {
                    Ok(text) => { ordered.insert(result.index, Some(text)); }
                    Err(message) => {
                        emit_error(&app, message);
                        ordered.insert(result.index, None);
                    }
                }
                while let Some(text) = ordered.remove(&next_result) {
                    if let Some(text) = text {
                        let _ = app.emit("system-audio-progress", SystemAudioEvent {
                            kind: "transcript",
                            elapsed_seconds: 0.0,
                            chunk_seconds: 0.0,
                            completed_chunks: next_result,
                            queued_chunks: pending,
                            active_requests: parallel_requests.saturating_sub(semaphore.available_permits()),
                            waiting_for_silence: false,
                            force_split_in_seconds: 0.0,
                            estimated_cost: 0.0,
                            chunk: Some(next_result),
                            text: Some(text),
                            message: None,
                            code: None,
                            input_level: None,
                        });
                    }
                    next_result += 1;
                }
                let active_requests = parallel_requests.saturating_sub(semaphore.available_permits());
                let _ = app.emit("system-audio-progress", SystemAudioEvent {
                    kind: "queue",
                    elapsed_seconds: 0.0,
                    chunk_seconds: 0.0,
                    completed_chunks: next_result.saturating_sub(1),
                    queued_chunks: pending.saturating_sub(active_requests),
                    active_requests,
                    waiting_for_silence: false,
                    force_split_in_seconds: 0.0,
                    estimated_cost: 0.0,
                    chunk: None,
                    text: None,
                    message: None,
                    code: None,
                    input_level: None,
                });
            }
            _ = queue_tick.tick(), if pending > 0 => {
                let active_requests = parallel_requests.saturating_sub(semaphore.available_permits());
                let _ = app.emit("system-audio-progress", SystemAudioEvent {
                    kind: "queue",
                    elapsed_seconds: 0.0,
                    chunk_seconds: 0.0,
                    completed_chunks: next_result.saturating_sub(1),
                    queued_chunks: pending.saturating_sub(active_requests),
                    active_requests,
                    waiting_for_silence: false,
                    force_split_in_seconds: 0.0,
                    estimated_cost: 0.0,
                    chunk: None,
                    text: None,
                    message: None,
                    code: None,
                    input_level: None,
                });
            }
        }
    }
    running.store(false, Ordering::Release);
    let _ = app.emit(
        "system-audio-progress",
        SystemAudioEvent {
            kind: "done",
            elapsed_seconds: 0.0,
            chunk_seconds: 0.0,
            completed_chunks: next_result.saturating_sub(1),
            queued_chunks: 0,
            active_requests: 0,
            waiting_for_silence: false,
            force_split_in_seconds: 0.0,
            estimated_cost: 0.0,
            chunk: None,
            text: None,
            message: None,
            code: None,
            input_level: None,
        },
    );
}

async fn transcribe_chunk(
    app: &tauri::AppHandle,
    temp: &tempfile::TempDir,
    chunk: AudioChunk,
    api_key: &str,
    model: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let wav = temp.path().join(format!("standard-{:04}.wav", chunk.index));
    let samples = chunk.samples;
    let wav_for_write = wav.clone();
    tokio::task::spawn_blocking(move || write_wav(&wav_for_write, &samples)).await??;
    let work = temp.path().join(format!("parts-{:04}", chunk.index));
    tokio::fs::create_dir_all(&work).await?;
    let parts = audio::prepare(&wav, false, 0, false, &work).await?;
    let mut transcript = String::new();
    for (part_index, part) in parts.into_iter().enumerate() {
        let prompt = (!transcript.is_empty()).then(|| tail_chars(&transcript, 800));
        let text = transcription::transcribe_recording_part(
            app,
            api_key,
            &part,
            model,
            prompt.as_deref(),
            chunk.index.saturating_add(part_index),
        )
        .await?;
        if !transcript.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(text.trim());
    }
    let _ = chunk.duration_seconds;
    Ok(transcript)
}

fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    let data_len = u32::try_from(std::mem::size_of_val(samples)).unwrap_or(u32::MAX);
    file.write_all(b"RIFF")?;
    file.write_all(&(36_u32.saturating_add(data_len)).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&CHANNELS.to_le_bytes())?;
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * 2;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&(CHANNELS * 2).to_le_bytes())?;
    file.write_all(&16_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

fn tail_chars(value: &str, count: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(count).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn emit_error(app: &tauri::AppHandle, message: String) {
    let code = crate::errors::classify(&message).code.to_owned();
    let _ = app.emit(
        "system-audio-progress",
        SystemAudioEvent {
            kind: "error",
            elapsed_seconds: 0.0,
            chunk_seconds: 0.0,
            completed_chunks: 0,
            queued_chunks: 0,
            active_requests: 0,
            waiting_for_silence: false,
            force_split_in_seconds: 0.0,
            estimated_cost: 0.0,
            chunk: None,
            text: None,
            message: Some(message),
            code: Some(code),
            input_level: None,
        },
    );
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{PcmBatch, CHANNELS, SAMPLE_RATE};
    use screencapturekit::prelude::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    };
    use std::time::Duration;

    struct Handler {
        sender: SyncSender<PcmBatch>,
    }

    impl SCStreamOutputTrait for Handler {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output: SCStreamOutputType) {
            if output != SCStreamOutputType::Audio || !sample.data_is_ready() {
                return;
            }
            let Some(format) = sample.format_description() else {
                return;
            };
            if format.audio_bits_per_channel() != Some(32) || !format.audio_is_float() {
                return;
            }
            let Ok(buffers) = sample.audio_buffer_list() else {
                return;
            };
            let channels = format.audio_channel_count().unwrap_or(CHANNELS as u32) as usize;
            let frames = sample.num_samples().max(0) as usize;
            let mut samples = Vec::with_capacity(frames * channels);
            if buffers.num_buffers() == 1 {
                if let Some(buffer) = buffers.get(0) {
                    for bytes in buffer.data().chunks_exact(4) {
                        samples.push(float_to_i16(f32::from_le_bytes(bytes.try_into().unwrap())));
                    }
                }
            } else {
                for frame in 0..frames {
                    for channel in 0..channels {
                        let Some(buffer) = buffers.get(channel) else {
                            return;
                        };
                        let offset = frame * 4;
                        let Some(bytes) = buffer.data().get(offset..offset + 4) else {
                            return;
                        };
                        samples.push(float_to_i16(f32::from_le_bytes(bytes.try_into().unwrap())));
                    }
                }
            }
            if !samples.is_empty() {
                let _ = self.sender.send(PcmBatch { samples });
            }
        }
    }

    fn float_to_i16(value: f32) -> i16 {
        (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
    }

    pub fn capture(stop: Arc<AtomicBool>, sender: SyncSender<PcmBatch>) -> Result<(), String> {
        let content = SCShareableContent::get().map_err(|error| error.to_string())?;
        let display = content
            .displays()
            .into_iter()
            .next()
            .ok_or("No display is available.")?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|error| error.to_string())?;
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_captures_audio(true)
            .with_sample_rate(SAMPLE_RATE as i32)
            .with_channel_count(CHANNELS as i32);
        let mut stream = SCStream::new(&filter, &config).map_err(|error| error.to_string())?;
        stream
            .add_output_handler(Handler { sender }, SCStreamOutputType::Audio)
            .map_err(|error| error.to_string())?;
        stream.start_capture().map_err(|error| error.to_string())?;
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
        }
        stream.stop_capture().map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::PcmBatch;
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::SyncSender,
            Arc,
        },
    };
    use wasapi::*;

    pub fn capture(stop: Arc<AtomicBool>, sender: SyncSender<PcmBatch>) -> Result<(), String> {
        initialize_mta().ok().map_err(|error| error.to_string())?;
        let enumerator = DeviceEnumerator::new().map_err(|error| error.to_string())?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .map_err(|error| error.to_string())?;
        let mut client = device
            .get_iaudioclient()
            .map_err(|error| error.to_string())?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
        let (_, minimum) = client
            .get_device_period()
            .map_err(|error| error.to_string())?;
        client
            .initialize_client(
                &format,
                &Direction::Render,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: minimum,
                },
            )
            .map_err(|error| error.to_string())?;
        let event = client
            .set_get_eventhandle()
            .map_err(|error| error.to_string())?;
        let capture = client
            .get_audiocaptureclient()
            .map_err(|error| error.to_string())?;
        let mut bytes = VecDeque::new();
        client.start_stream().map_err(|error| error.to_string())?;
        while !stop.load(Ordering::Acquire) {
            capture
                .read_from_device_to_deque(&mut bytes)
                .map_err(|error| error.to_string())?;
            let available = bytes.len() / 4 * 4;
            if available > 0 {
                let raw: Vec<u8> = bytes.drain(..available).collect();
                let samples = raw
                    .chunks_exact(4)
                    .map(|value| {
                        (f32::from_le_bytes(value.try_into().unwrap()).clamp(-1.0, 1.0)
                            * i16::MAX as f32)
                            .round() as i16
                    })
                    .collect();
                sender
                    .send(PcmBatch { samples })
                    .map_err(|error| error.to_string())?;
            }
            let _ = event.wait_for_event(100);
        }
        client.stop_stream().map_err(|error| error.to_string())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::PcmBatch;
    use std::sync::{atomic::AtomicBool, mpsc::SyncSender, Arc};

    pub fn capture(_: Arc<AtomicBool>, _: SyncSender<PcmBatch>) -> Result<(), String> {
        Err("System Audio Standard Mode is available only on macOS and Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_until_target_then_splits_on_silence() {
        let mut chunker = SilenceChunker::default();
        let speech = vec![2_000; 90 * SAMPLE_RATE as usize * CHANNELS as usize];
        assert!(chunker.push(speech).is_empty());
        let silence = vec![0; SAMPLE_RATE as usize / 2 * CHANNELS as usize];
        let chunks = chunker.push(silence);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].duration_seconds >= 90.0);
    }

    #[test]
    fn early_silence_does_not_split() {
        let mut chunker = SilenceChunker::default();
        let silence = vec![0; 60 * SAMPLE_RATE as usize * CHANNELS as usize];
        assert!(chunker.push(silence).is_empty());
    }

    #[test]
    fn forces_a_split_at_150_seconds() {
        let mut chunker = SilenceChunker::default();
        let speech = vec![2_000; FORCE_SECONDS as usize * SAMPLE_RATE as usize * CHANNELS as usize];
        let chunks = chunker.push(speech);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].duration_seconds, 150.0);
    }

    #[test]
    fn combines_system_and_microphone_without_clipping() {
        let mut system = std::collections::VecDeque::from(vec![30_000; MIX_BLOCK_SAMPLES]);
        let mut microphone = std::collections::VecDeque::from(vec![30_000; MIX_BLOCK_SAMPLES]);
        let mixed = mix_audio_block(&mut system, &mut microphone);
        assert_eq!(mixed.len(), MIX_BLOCK_SAMPLES);
        assert!(mixed.iter().all(|sample| *sample == 30_000));
        assert!(system.is_empty() && microphone.is_empty());
    }
}
