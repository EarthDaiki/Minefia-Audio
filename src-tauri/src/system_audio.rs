use crate::{audio, transcription};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
};
use tauri::Emitter;
use tokio::sync::{mpsc as tokio_mpsc, Semaphore};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 1;
const TARGET_SECONDS: u64 = 90;
const FORCE_SECONDS: u64 = 150;
const SILENCE_SECONDS: f64 = 0.5;
const SILENCE_AMPLITUDE: i16 = 583; // 20 * log10(583 / 32767) ~= -35 dBFS
const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MIC_RING_CAPACITY_SAMPLES: usize = 384_000;
const MIC_PROCESS_BATCH_SAMPLES: usize = 4_096;
#[cfg(test)]
const MIX_BLOCK_SAMPLES: usize = 480; // 10 ms at 48 kHz mono.

#[derive(Clone, Copy, Debug)]
pub(crate) struct CaptureProfile {
    pub(crate) key: &'static str,
    pub(crate) sample_rate: u32,
    pub(crate) bitrate_kbps: u32,
}

impl CaptureProfile {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "low" => Ok(Self {
                key: "low",
                sample_rate: 16_000,
                bitrate_kbps: 32,
            }),
            "standard" => Ok(Self {
                key: "standard",
                sample_rate: 16_000,
                bitrate_kbps: 64,
            }),
            "high" => Ok(Self {
                key: "high",
                sample_rate: 24_000,
                bitrate_kbps: 96,
            }),
            "best" => Ok(Self {
                key: "best",
                sample_rate: 48_000,
                bitrate_kbps: 128,
            }),
            _ => Err("Choose a supported audio quality preset.".into()),
        }
    }
}

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
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
}

// Keep the device's native sample representation in the real-time callback.
// Conversion to the common f32 processing format happens on the ring consumer.
#[derive(Clone, Copy)]
enum RawMicrophoneSample {
    F32(f32),
    I16(i16),
    I32(i32),
    U16(u16),
}

impl RawMicrophoneSample {
    fn to_f32(self) -> f32 {
        match self {
            Self::F32(sample) => sample,
            Self::I16(sample) => i16_to_f32(sample),
            Self::I32(sample) => i32_to_f32(sample),
            Self::U16(sample) => u16_to_f32(sample),
        }
    }
}

struct AudioChunk {
    index: usize,
    samples: Vec<i16>,
    duration_seconds: f64,
    forced: bool,
    profile: CaptureProfile,
    compact: bool,
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
    pub supported_sample_rates: Vec<u32>,
    pub max_supported_sample_rate: Option<u32>,
    pub supported_channels: Vec<u16>,
    pub default_sample_rate: Option<u32>,
    pub default_channels: Option<u16>,
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
            let name = device.name().ok()?;
            let default = device.default_input_config().ok();
            let mut supported_sample_rates = vec![];
            let mut max_supported_sample_rate: Option<u32> = None;
            let mut supported_channels = vec![];
            if let Ok(configs) = device.supported_input_configs() {
                for config in configs {
                    if !matches!(
                        config.sample_format(),
                        cpal::SampleFormat::F32
                            | cpal::SampleFormat::I16
                            | cpal::SampleFormat::I32
                            | cpal::SampleFormat::U16
                    ) {
                        continue;
                    }
                    max_supported_sample_rate = Some(
                        max_supported_sample_rate
                            .unwrap_or_default()
                            .max(config.max_sample_rate().0),
                    );
                    let channels = config.channels() as u16;
                    if !supported_channels.contains(&channels) {
                        supported_channels.push(channels);
                    }
                    for rate in [16_000, 24_000, 48_000] {
                        if config.min_sample_rate().0 <= rate
                            && rate <= config.max_sample_rate().0
                            && !supported_sample_rates.contains(&rate)
                        {
                            supported_sample_rates.push(rate);
                        }
                    }
                }
            }
            supported_sample_rates.sort_unstable();
            supported_channels.sort_unstable();
            Some(InputDevice {
                id: index.to_string(),
                name,
                supported_sample_rates,
                max_supported_sample_rate: max_supported_sample_rate
                    .or_else(|| default.as_ref().map(|config| config.sample_rate().0)),
                supported_channels,
                default_sample_rate: default.as_ref().map(|config| config.sample_rate().0),
                default_channels: default.as_ref().map(|config| config.channels() as u16),
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
    quality: String,
    compact_microphone: bool,
    compact_system_audio: bool,
) -> Result<(), String> {
    let profile = CaptureProfile::parse(&quality)?;
    validate_source(&source, input_device.as_deref())?;
    let compact_capture = match source.as_str() {
        "system" => compact_system_audio,
        "microphone" => compact_microphone,
        "both" => compact_system_audio || compact_microphone,
        _ => false,
    };
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
        .spawn(move || {
            process_audio(
                batch_rx,
                chunk_tx,
                processor_app,
                price_per_minute,
                profile,
                compact_capture,
            )
        })
        .map_err(|error| error.to_string())?;

    let capture_app = app.clone();
    let capture_stop = stop.clone();
    std::thread::Builder::new()
        .name("minefia-system-audio".into())
        .spawn(move || {
            // Keep the original sender alive until capture_stopped has been emitted.
            // Otherwise process_audio can observe a closed channel and the transcription
            // queue may emit `done` first; the late stop event then leaves the UI spinning.
            let result = capture_source(
                capture_stop,
                batch_tx.clone(),
                &source,
                input_device.as_deref(),
                &capture_app,
                profile,
                compact_microphone,
                compact_system_audio,
            );
            if let Err(message) = result {
                emit_error(&capture_app, message);
            }
            // The remaining finalized chunks still drain through the transcription queue.
            let _ = capture_app.emit(
                "system-audio-progress",
                SystemAudioEvent {
                    kind: "capture_stopped",
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
                    message: Some("Audio input stopped; finishing captured parts.".into()),
                    code: None,
                    input_level: None,
                },
            );
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
    profile: CaptureProfile,
    compact_microphone: bool,
    compact_system_audio: bool,
) -> Result<(), String> {
    let recordings = crate::storage::dated_recording_dir(app)?;
    std::fs::create_dir_all(&recordings).map_err(|error| error.to_string())?;
    if source == "both" {
        return capture_both_sources(
            stop,
            sender,
            input_device,
            app,
            recordings,
            profile,
            compact_system_audio || compact_microphone,
        );
    }
    let (capture_tx, capture_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let capture_stop = stop.clone();
    let capture_app = app.clone();
    let source = source.to_owned();
    let use_compact_aac = if source == "system" {
        compact_system_audio
    } else {
        compact_microphone
    };
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
                    profile.sample_rate,
                    use_compact_aac,
                )
            } else {
                platform::capture(capture_stop, capture_tx, profile.sample_rate)
            }
        })
        .map_err(|error| error.to_string())?;

    let mut recorder = CaptureRecorder::new(recordings, profile, use_compact_aac)?;
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
    if let Some(path) = saved {
        let _ = app.emit("audio-recordings-saved", vec![path]);
    }
    if let Some(error) = receiver_error {
        return Err(error);
    }
    capture_result
}

// Persist generated capture PCM in the app-data directory while it is streamed onward.
// Rotate before the RIFF/WAV 32-bit size limit rather than allowing an invalid large file.
enum CaptureRecorder {
    Aac(CaptureAacRecorder),
    Pcm(CapturePcmRecorder),
}

impl CaptureRecorder {
    fn new(
        directory: std::path::PathBuf,
        profile: CaptureProfile,
        compact: bool,
    ) -> Result<Self, String> {
        if compact {
            CaptureAacRecorder::new(directory, profile).map(Self::Aac)
        } else {
            CapturePcmRecorder::new(directory, profile.sample_rate).map(Self::Pcm)
        }
    }

    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        match self {
            Self::Aac(recorder) => recorder.write(samples),
            Self::Pcm(recorder) => recorder.write(samples),
        }
    }

    fn finish(self) -> Result<Option<String>, String> {
        match self {
            Self::Aac(recorder) => recorder.finish(),
            Self::Pcm(recorder) => recorder.finish(),
        }
    }
}

struct CapturePcmRecorder {
    file: Option<File>,
    partial_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
    sample_rate: u32,
    data_bytes: u64,
}

impl CapturePcmRecorder {
    fn new(directory: std::path::PathBuf, sample_rate: u32) -> Result<Self, String> {
        let timestamp = crate::storage::timestamp_millis();
        let final_path = directory.join(format!("rec_{timestamp}.wav"));
        let partial_path = directory.join(format!("rec_{timestamp}.wav.part"));
        let mut file = File::create(&partial_path).map_err(|error| error.to_string())?;
        write_wav_header(&mut file, sample_rate, 0).map_err(|error| error.to_string())?;
        Ok(Self {
            file: Some(file),
            partial_path,
            final_path,
            sample_rate,
            data_bytes: 0,
        })
    }

    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let incoming = (samples.len() as u64) * 2;
        if self.data_bytes + incoming > u32::MAX as u64 {
            return Err("Uncompressed WAV reached its 4 GB format limit. Enable Compact to continue with AAC.".into());
        }
        let file = self
            .file
            .as_mut()
            .ok_or("WAV recording has already ended.")?;
        for sample in samples {
            file.write_all(&sample.to_le_bytes())
                .map_err(|error| error.to_string())?;
        }
        self.data_bytes += incoming;
        Ok(())
    }

    fn finish(mut self) -> Result<Option<String>, String> {
        let mut file = self.file.take().ok_or("WAV recording is unavailable.")?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        write_wav_header(&mut file, self.sample_rate, self.data_bytes as u32)
            .map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&self.partial_path, &self.final_path).map_err(|error| error.to_string())?;
        Ok(Some(self.final_path.to_string_lossy().into_owned()))
    }
}

fn write_wav_header(file: &mut File, sample_rate: u32, data_len: u32) -> std::io::Result<()> {
    file.write_all(b"RIFF")?;
    file.write_all(&(36_u32.saturating_add(data_len)).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&CHANNELS.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate * CHANNELS as u32 * 2).to_le_bytes())?;
    file.write_all(&(CHANNELS * 2).to_le_bytes())?;
    file.write_all(&16_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())
}

struct CaptureAacRecorder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    output_thread: Option<JoinHandle<std::io::Result<()>>>,
    partial_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
}

impl CaptureAacRecorder {
    fn new(directory: std::path::PathBuf, profile: CaptureProfile) -> Result<Self, String> {
        let ffmpeg = audio::find_binary("ffmpeg").ok_or("FFmpeg is required for AAC recording.")?;
        let timestamp = crate::storage::timestamp_millis();
        let final_path = directory.join(format!("rec_{timestamp}.m4a"));
        let partial_path = directory.join(format!("rec_{timestamp}.m4a.part"));
        let mut child = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "s16le",
                "-ar",
                &profile.sample_rate.to_string(),
                "-ac",
                "1",
                "-i",
                "pipe:0",
                "-vn",
                "-c:a",
                "aac",
                "-b:a",
                &format!("{}k", profile.bitrate_kbps),
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
                "-frag_duration",
                "1000000",
                "-f",
                "mp4",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start AAC encoder: {error}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or("AAC encoder output was unavailable.")?;
        let output_path = partial_path.clone();
        let output_thread = std::thread::Builder::new()
            .name("minefia-aac-file-writer".into())
            .spawn(move || {
                let mut file = File::create(output_path)?;
                std::io::copy(&mut stdout, &mut file)?;
                file.flush()
            })
            .map_err(|error| format!("Could not start AAC file writer: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("AAC encoder input was unavailable.")?;
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            output_thread: Some(output_thread),
            partial_path,
            final_path,
        })
    }

    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("AAC recording has already ended.")?;
        for sample in samples {
            stdin
                .write_all(&sample.to_le_bytes())
                .map_err(|error| format!("AAC encoder input failed: {error}"))?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Option<String>, String> {
        self.stdin.take();
        let output = self
            .child
            .take()
            .ok_or("AAC encoder is unavailable.")?
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        let writer = self
            .output_thread
            .take()
            .ok_or("AAC file writer is unavailable.")?
            .join()
            .map_err(|_| "AAC file writer stopped unexpectedly.")?;
        writer.map_err(|error| format!("Could not save compressed recording: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "AAC encoder failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if !self.partial_path.exists()
            || std::fs::metadata(&self.partial_path)
                .map_err(|error| error.to_string())?
                .len()
                == 0
        {
            return Err("AAC encoder produced an empty recording.".into());
        }
        std::fs::rename(&self.partial_path, &self.final_path).map_err(|error| error.to_string())?;
        Ok(Some(self.final_path.to_string_lossy().into_owned()))
    }
}

impl Drop for CaptureAacRecorder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
    profile: CaptureProfile,
    compact_microphone: bool,
) -> Result<(), String> {
    let mut recorder = CaptureRecorder::new(recordings, profile, compact_microphone)?;
    let (system_tx, system_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let (microphone_tx, microphone_rx) = mpsc::sync_channel::<PcmBatch>(64);
    let system_stop = stop.clone();
    let system_worker = std::thread::Builder::new()
        .name("minefia-system-audio-source".into())
        .spawn(move || platform::capture(system_stop, system_tx, profile.sample_rate))
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
                profile.sample_rate,
                compact_microphone,
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
        let block_samples = (profile.sample_rate as usize / 100) * CHANNELS as usize;
        let max_mix_buffer_samples = profile.sample_rate as usize * CHANNELS as usize * 5;
        if !stop.load(Ordering::Acquire)
            && (system_samples.len() > max_mix_buffer_samples
                || microphone_samples.len() > max_mix_buffer_samples)
        {
            failure = Some("Combined audio input exceeded its safe synchronization buffer.".into());
            stop.store(true, Ordering::Release);
        }
        while system_samples.len() >= block_samples && microphone_samples.len() >= block_samples {
            let mixed =
                mix_audio_block(&mut system_samples, &mut microphone_samples, block_samples);
            let samples = mixed.clone();
            if let Err(error) = recorder.write(&samples) {
                failure = Some(error);
                stop.store(true, Ordering::Release);
                break;
            }
            if sender
                .send(PcmBatch {
                    samples,
                    sample_rate: profile.sample_rate,
                    channels: CHANNELS,
                })
                .is_err()
            {
                failure = Some("The audio processing queue closed during combined capture.".into());
                stop.store(true, Ordering::Release);
                break;
            }
        }
        if stop.load(Ordering::Acquire) && system_closed && microphone_closed {
            while !system_samples.is_empty() || !microphone_samples.is_empty() {
                let mixed =
                    mix_audio_block(&mut system_samples, &mut microphone_samples, block_samples);
                let samples = mixed.clone();
                if let Err(error) = recorder.write(&samples) {
                    failure = Some(error);
                    break;
                }
                if sender
                    .send(PcmBatch {
                        samples,
                        sample_rate: profile.sample_rate,
                        channels: CHANNELS,
                    })
                    .is_err()
                {
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
    match recorder.finish() {
        Ok(Some(path)) => {
            let _ = app.emit("audio-recordings-saved", vec![path]);
        }
        Ok(None) => {}
        Err(error) => {
            failure.get_or_insert(error);
        }
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
    samples_per_block: usize,
) -> Vec<i16> {
    (0..samples_per_block)
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
    target_rate: u32,
    compact_output: bool,
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
    // Prefer a native mono stream at the requested rate; some devices only expose
    // stereo or a fixed hardware rate, in which case the callback converts it.
    let mut max_supported_rate = 0_u32;
    let preferred = device.supported_input_configs().ok().and_then(|configs| {
        let mut candidates: Vec<_> = configs
            .filter(|config| {
                matches!(
                    config.sample_format(),
                    cpal::SampleFormat::F32
                        | cpal::SampleFormat::I16
                        | cpal::SampleFormat::I32
                        | cpal::SampleFormat::U16
                )
            })
            .filter_map(|config| {
                let min_rate = config.min_sample_rate().0;
                let max_rate = config.max_sample_rate().0;
                max_supported_rate = max_supported_rate.max(max_rate);
                let rate = target_rate.clamp(min_rate, max_rate);
                let supported = config.try_with_sample_rate(cpal::SampleRate(rate))?;
                Some((
                    config.channels() != 1,
                    rate.abs_diff(target_rate),
                    match config.sample_format() {
                        cpal::SampleFormat::F32 => 0,
                        cpal::SampleFormat::I16 => 1,
                        cpal::SampleFormat::I32 => 2,
                        _ => 2,
                    },
                    supported,
                ))
            })
            .collect();
        candidates.sort_by_key(|(not_mono, rate_delta, format_rank, _)| {
            (*not_mono, *rate_delta, *format_rank)
        });
        candidates
            .into_iter()
            .next()
            .map(|(_, _, _, config)| config)
    });
    let default = if max_supported_rate == 0 {
        device.default_input_config().ok()
    } else {
        None
    };
    let max_supported_rate = if max_supported_rate > 0 {
        max_supported_rate
    } else {
        default.as_ref().map_or(0, |config| config.sample_rate().0)
    };
    if target_rate > max_supported_rate {
        return Err(format!(
            "The selected microphone supports up to {max_supported_rate} Hz, below the requested {target_rate} Hz. Choose a lower quality or a microphone with a higher supported sample rate."
        ));
    }
    let supported = match preferred {
        Some(config) => config,
        None => default
            .or_else(|| device.default_input_config().ok())
            .ok_or("Could not open microphone: no usable input format was reported.")?,
    };
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    if channels == 0 || sample_rate == 0 {
        return Err("The microphone reported an invalid audio format.".into());
    }
    if target_rate > sample_rate {
        return Err(format!(
            "The selected microphone opened at {sample_rate} Hz, below the requested {target_rate} Hz. Choose a lower quality to avoid upsampling."
        ));
    }
    let config: cpal::StreamConfig = supported.clone().into();
    // The input's native channel count and rate are captured explicitly, then
    // normalized to the selected mono AAC profile before persistence/upload.
    let _ = app.emit(
        "audio-capture-format",
        serde_json::json!({
            "source": "microphone",
            "sampleRate": sample_rate,
            "channels": channels,
            "targetSampleRate": target_rate,
            "targetChannels": CHANNELS,
            "captureEncoding": if compact_output { "AAC" } else { "PCM/WAV" },
        }),
    );
    let stream_error = Arc::new(Mutex::new(None::<String>));
    let callback_error = stream_error.clone();
    // The callback only copies native samples into the preallocated ring; the
    // consumer thread performs format conversion and channel/rate processing.
    let (mut ring_producer, ring_consumer) =
        rtrb::RingBuffer::<RawMicrophoneSample>::new(MIC_RING_CAPACITY_SAMPLES);
    let processing_stop = Arc::new(AtomicBool::new(false));
    let overflowed = Arc::new(AtomicBool::new(false));
    let callback_stop = stop.clone();
    let callback_overflow = overflowed.clone();
    let stream_error_callback = move |error| {
        if let Ok(mut stored) = callback_error.lock() {
            *stored = Some(format!("Microphone disconnected or failed: {error}"));
        }
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                enqueue_microphone_samples(
                    &mut ring_producer,
                    data.iter().copied().map(RawMicrophoneSample::F32),
                    &callback_stop,
                    &callback_overflow,
                );
            },
            stream_error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => {
            let callback_stop = stop.clone();
            let callback_overflow = overflowed.clone();
            let stream_error = stream_error.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    enqueue_microphone_samples(
                        &mut ring_producer,
                        data.iter().copied().map(RawMicrophoneSample::I16),
                        &callback_stop,
                        &callback_overflow,
                    );
                },
                move |error| {
                    if let Ok(mut stored) = stream_error.lock() {
                        *stored = Some(format!("Microphone disconnected or failed: {error}"));
                    }
                },
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let callback_stop = stop.clone();
            let callback_overflow = overflowed.clone();
            let stream_error = stream_error.clone();
            device.build_input_stream(
                &config,
                move |data: &[i32], _| {
                    enqueue_microphone_samples(
                        &mut ring_producer,
                        data.iter().copied().map(RawMicrophoneSample::I32),
                        &callback_stop,
                        &callback_overflow,
                    );
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
            let callback_stop = stop.clone();
            let callback_overflow = overflowed.clone();
            let stream_error = stream_error.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    enqueue_microphone_samples(
                        &mut ring_producer,
                        data.iter().copied().map(RawMicrophoneSample::U16),
                        &callback_stop,
                        &callback_overflow,
                    );
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

    let processor_stop = processing_stop.clone();
    let processor_overflow = overflowed.clone();
    let processor_app = app.clone();
    let processor = std::thread::Builder::new()
        .name("minefia-microphone-ring-consumer".into())
        .spawn(move || {
            process_microphone_ring(
                ring_consumer,
                processor_stop,
                processor_overflow,
                sender,
                sample_rate,
                channels,
                target_rate,
                &processor_app,
            )
        })
        .map_err(|error| format!("Could not start microphone audio processor: {error}"))?;

    if let Err(error) = stream.play() {
        drop(stream);
        processing_stop.store(true, Ordering::Release);
        let _ = processor.join();
        return Err(format!("Microphone permission or device error: {error}"));
    }
    let mut capture_error = None;
    while !stop.load(Ordering::Acquire) {
        if let Ok(mut stored) = stream_error.lock() {
            if let Some(error) = stored.take() {
                capture_error = Some(error);
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(stream);
    processing_stop.store(true, Ordering::Release);
    processor
        .join()
        .map_err(|_| "Microphone audio processor stopped unexpectedly.".to_owned())??;
    if overflowed.load(Ordering::Acquire) {
        return Err("Microphone audio ring buffer filled; capture stopped to prevent blocking the audio callback and silently losing samples.".into());
    }
    capture_error.map_or(Ok(()), Err)
}

fn i16_to_f32(sample: i16) -> f32 {
    (sample as f32 / i16::MAX as f32).clamp(-1.0, 1.0)
}

fn i32_to_f32(sample: i32) -> f32 {
    (sample as f32 / i32::MAX as f32).clamp(-1.0, 1.0)
}

fn u16_to_f32(sample: u16) -> f32 {
    (sample as f32 - 32768.0) / 32768.0
}

fn enqueue_microphone_samples(
    producer: &mut rtrb::Producer<RawMicrophoneSample>,
    samples: impl Iterator<Item = RawMicrophoneSample>,
    stop: &AtomicBool,
    overflowed: &AtomicBool,
) {
    for sample in samples {
        if producer.push(sample).is_err() {
            overflowed.store(true, Ordering::Release);
            stop.store(true, Ordering::Release);
            break;
        }
    }
}

fn process_microphone_ring(
    mut consumer: rtrb::Consumer<RawMicrophoneSample>,
    stop: Arc<AtomicBool>,
    overflowed: Arc<AtomicBool>,
    sender: mpsc::SyncSender<PcmBatch>,
    source_rate: u32,
    source_channels: usize,
    target_rate: u32,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let mut raw = Vec::with_capacity(MIC_PROCESS_BATCH_SAMPLES + source_channels);
    let mut input = Vec::with_capacity(MIC_PROCESS_BATCH_SAMPLES);
    let mut phase = 0_u64;
    loop {
        let mut popped = 0;
        while popped < MIC_PROCESS_BATCH_SAMPLES {
            match consumer.pop() {
                Ok(sample) => {
                    raw.push(sample.to_f32());
                    popped += 1;
                }
                Err(_) => break,
            }
        }
        let process_len = raw.len() / source_channels * source_channels;
        if process_len > 0 {
            input.clear();
            input.extend(raw[..process_len].iter().copied());
            process_microphone_samples(
                &input,
                source_rate,
                source_channels,
                &mut phase,
                &sender,
                app,
                target_rate,
            );
            let remaining = raw.len() - process_len;
            raw.copy_within(process_len.., 0);
            raw.truncate(remaining);
        }
        if stop.load(Ordering::Acquire) && consumer.is_empty() {
            break;
        }
        if popped == 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    if overflowed.load(Ordering::Acquire) {
        return Err("Microphone audio ring buffer overflowed during capture.".into());
    }
    Ok(())
}

fn process_microphone_samples(
    input: &[f32],
    source_rate: u32,
    source_channels: usize,
    phase: &mut u64,
    sender: &mpsc::SyncSender<PcmBatch>,
    app: &tauri::AppHandle,
    target_rate: u32,
) {
    let mut output = Vec::with_capacity(
        input.len() / source_channels * target_rate as usize / source_rate as usize + 1,
    );
    let mut level_sum = 0.0_f64;
    let mut source_frames = 0usize;
    for frame in input.chunks_exact(source_channels) {
        let left = frame[0].clamp(-1.0, 1.0);
        let mono = if source_channels > 1 {
            (frame
                .iter()
                .map(|sample| sample.clamp(-1.0, 1.0) as f64)
                .sum::<f64>()
                / source_channels as f64) as f32
        } else {
            left
        };
        level_sum += (mono as f64).powi(2);
        source_frames += 1;
        *phase += u64::from(target_rate);
        while *phase >= source_rate as u64 {
            output.push((mono * i16::MAX as f32).round() as i16);
            *phase -= source_rate as u64;
        }
    }
    if source_frames > 0 {
        let level = (level_sum / source_frames as f64).sqrt() as f32;
        emit_input_level(app, level);
    }
    if !output.is_empty() {
        if sender
            .send(PcmBatch {
                samples: output,
                sample_rate: target_rate,
                channels: CHANNELS,
            })
            .is_err()
        {
            return;
        }
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
    profile: CaptureProfile,
    compact_microphone: bool,
) {
    let mut chunker = SilenceChunker::with_profile(profile, compact_microphone);
    while let Ok(batch) = batches.recv() {
        debug_assert_eq!(
            batch.channels, CHANNELS,
            "capture sources must provide mono PCM"
        );
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

struct SilenceChunker {
    buffer: Vec<i16>,
    silent_frames: usize,
    total_frames: u64,
    next_chunk_index: usize,
    last_status_frame: u64,
    sample_rate: u32,
    profile: CaptureProfile,
    compact: bool,
}

impl Default for SilenceChunker {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            silent_frames: 0,
            total_frames: 0,
            next_chunk_index: 0,
            last_status_frame: 0,
            sample_rate: SAMPLE_RATE,
            profile: CaptureProfile {
                key: "best",
                sample_rate: SAMPLE_RATE,
                bitrate_kbps: 128,
            },
            compact: true,
        }
    }
}

impl SilenceChunker {
    fn with_profile(profile: CaptureProfile, compact: bool) -> Self {
        Self {
            sample_rate: profile.sample_rate,
            profile,
            compact,
            ..Self::default()
        }
    }

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
            let target_frames = TARGET_SECONDS as usize * self.sample_rate as usize;
            let force_frames = FORCE_SECONDS as usize * self.sample_rate as usize;
            let silence_frames = (SILENCE_SECONDS * self.sample_rate as f64) as usize;
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
            duration_seconds: frames as f64 / self.sample_rate as f64,
            samples,
            forced,
            profile: self.profile,
            compact: self.compact,
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
        if self.total_frames.saturating_sub(self.last_status_frame) < self.sample_rate as u64 / 4 {
            return;
        }
        self.last_status_frame = self.total_frames;
        let elapsed = self.total_frames as f64 / self.sample_rate as f64;
        let current = self.buffer.len() as f64 / CHANNELS as f64 / self.sample_rate as f64;
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
                            queued_chunks: pending.saturating_sub(
                                parallel_requests.saturating_sub(semaphore.available_permits()),
                            ) + chunks.len(),
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
                    queued_chunks: pending.saturating_sub(active_requests) + chunks.len(),
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
                    queued_chunks: pending.saturating_sub(active_requests) + chunks.len(),
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
    let extension = if chunk.compact { "m4a" } else { "wav" };
    let media = temp
        .path()
        .join(format!("standard-{:04}.{extension}", chunk.index));
    let samples = chunk.samples;
    let profile = chunk.profile;
    let compact = chunk.compact;
    let media_for_write = media.clone();
    tokio::task::spawn_blocking(move || {
        if compact {
            encode_aac_file(&media_for_write, &samples, profile)
        } else {
            write_wav_file(&media_for_write, &samples, profile.sample_rate)
                .map_err(|error| error.to_string())
        }
    })
    .await??;
    let work = temp.path().join(format!("parts-{:04}", chunk.index));
    tokio::fs::create_dir_all(&work).await?;
    let parts = audio::prepare(&media, false, profile.key, 0, false, &work).await?;
    let mut transcript = String::new();
    for part in parts {
        let prompt = (!transcript.is_empty()).then(|| tail_chars(&transcript, 800));
        let text = transcription::transcribe_recording_part(
            app,
            api_key,
            &part,
            model,
            prompt.as_deref(),
            chunk.index,
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

fn write_wav_file(path: &Path, samples: &[i16], sample_rate: u32) -> std::io::Result<()> {
    let data_bytes = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut file = File::create(path)?;
    write_wav_header(&mut file, sample_rate, data_bytes)?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

fn encode_aac_file(path: &Path, samples: &[i16], profile: CaptureProfile) -> Result<(), String> {
    let ffmpeg =
        audio::find_binary("ffmpeg").ok_or("FFmpeg is required for AAC transcription chunks.")?;
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "s16le",
            "-ar",
            &profile.sample_rate.to_string(),
            "-ac",
            "1",
            "-i",
            "pipe:0",
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            &format!("{}k", profile.bitrate_kbps),
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        for sample in samples {
            stdin
                .write_all(&sample.to_le_bytes())
                .map_err(|error| error.to_string())?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "AAC chunk encoding failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
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
    use super::{PcmBatch, CHANNELS};
    use screencapturekit::prelude::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    };
    use std::time::Duration;

    struct Handler {
        sender: SyncSender<PcmBatch>,
        sample_rate: u32,
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
                let _ = self.sender.send(PcmBatch {
                    samples,
                    sample_rate: self.sample_rate,
                    channels: CHANNELS,
                });
            }
        }
    }

    fn float_to_i16(value: f32) -> i16 {
        (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
    }

    pub fn capture(
        stop: Arc<AtomicBool>,
        sender: SyncSender<PcmBatch>,
        sample_rate: u32,
    ) -> Result<(), String> {
        match capture_with_process_tap(stop.clone(), sender.clone(), sample_rate) {
            Ok(()) => Ok(()),
            Err(CaptureError::UnsupportedOsVersion) => {
                capture_with_screen_capture_kit(stop, sender, sample_rate)
            }
            Err(CaptureError::Failed(message)) => Err(message),
        }
    }

    enum CaptureError {
        UnsupportedOsVersion,
        Failed(String),
    }

    fn capture_with_process_tap(
        stop: Arc<AtomicBool>,
        sender: SyncSender<PcmBatch>,
        sample_rate: u32,
    ) -> Result<(), CaptureError> {
        use flexaudio::{OutputFormat, SourceKind, StreamConfig};

        let config = StreamConfig {
            kind: SourceKind::SystemLoopback,
            output: OutputFormat {
                sample_rate,
                channels: CHANNELS,
            },
            exclude_self: true,
            // Do not inject diagnostic audio into recordings.
            silence_probe: false,
            ..StreamConfig::default()
        };
        let mut stream = match flexaudio::open(config) {
            Ok(stream) => stream,
            Err(flexaudio::Error::UnsupportedOsVersion) => {
                return Err(CaptureError::UnsupportedOsVersion)
            }
            Err(error) => {
                return Err(CaptureError::Failed(format!(
                    "Core Audio Process Tap could not start: {error}"
                )))
            }
        };
        stream.start().map_err(|error| {
            CaptureError::Failed(format!("Core Audio Process Tap could not start: {error}"))
        })?;

        while !stop.load(Ordering::Acquire) {
            if let Some(error) = stream.terminal_error() {
                stream.stop();
                return Err(CaptureError::Failed(format!(
                    "Core Audio Process Tap stopped: {error}"
                )));
            }

            let mut received_audio = false;
            while let Some(chunk) = stream.poll_chunk() {
                received_audio = true;
                let samples = chunk.data.into_iter().map(float_to_i16).collect::<Vec<_>>();
                if !samples.is_empty()
                    && sender
                        .send(PcmBatch {
                            samples,
                            sample_rate,
                            channels: CHANNELS,
                        })
                        .is_err()
                {
                    stream.stop();
                    return Ok(());
                }
            }
            if !received_audio {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        stream.stop();
        Ok(())
    }

    fn capture_with_screen_capture_kit(
        stop: Arc<AtomicBool>,
        sender: SyncSender<PcmBatch>,
        sample_rate: u32,
    ) -> Result<(), String> {
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
            .with_sample_rate(sample_rate as i32)
            .with_channel_count(CHANNELS as i32);
        let mut stream = SCStream::new(&filter, &config).map_err(|error| error.to_string())?;
        stream
            .add_output_handler(
                Handler {
                    sender,
                    sample_rate,
                },
                SCStreamOutputType::Audio,
            )
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
    use super::{PcmBatch, CHANNELS};
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::SyncSender,
            Arc,
        },
    };
    use wasapi::*;

    pub fn capture(
        stop: Arc<AtomicBool>,
        sender: SyncSender<PcmBatch>,
        sample_rate: u32,
    ) -> Result<(), String> {
        initialize_mta().ok().map_err(|error| error.to_string())?;
        let enumerator = DeviceEnumerator::new().map_err(|error| error.to_string())?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .map_err(|error| error.to_string())?;
        let mut client = device
            .get_iaudioclient()
            .map_err(|error| error.to_string())?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, sample_rate, CHANNELS, None);
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
                    .send(PcmBatch {
                        samples,
                        sample_rate,
                        channels: CHANNELS,
                    })
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

    pub fn capture(_: Arc<AtomicBool>, _: SyncSender<PcmBatch>, _: u32) -> Result<(), String> {
        Err("System Audio Standard Mode is available only on macOS and Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_microphone_formats_normalize_to_f32_pcm() {
        assert_eq!(i16_to_f32(0), 0.0);
        assert_eq!(i16_to_f32(i16::MAX), 1.0);
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        assert_eq!(i32_to_f32(i32::MAX), 1.0);
        assert_eq!(i32_to_f32(i32::MIN), -1.0);
        assert_eq!(u16_to_f32(0), -1.0);
        assert_eq!(u16_to_f32(u16::MAX), 32767.0 / 32768.0);
    }

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
        let mixed = mix_audio_block(&mut system, &mut microphone, MIX_BLOCK_SAMPLES);
        assert_eq!(mixed.len(), MIX_BLOCK_SAMPLES);
        assert!(mixed.iter().all(|sample| *sample == 30_000));
        assert!(system.is_empty() && microphone.is_empty());
    }
}
