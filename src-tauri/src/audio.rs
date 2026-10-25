use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    env,
    path::{Path, PathBuf},
    process::Stdio,
};
use thiserror::Error;
use tokio::process::Command;

pub const MAX_UPLOAD_BYTES: u64 = 20_000_000;
const SAFE_UPLOAD_BYTES: u64 = 19_000_000;
const TARGET_DURATION_SECONDS: f64 = 5.0 * 60.0;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("The audio file could not be found.")]
    MissingFile,
    #[error("FFmpeg could not be found. Follow the setup instructions in README.md.")]
    FfmpegMissing,
    #[error("Could not inspect the audio file: {0}")]
    Probe(String),
    #[error("NO_AUDIO_STREAM")]
    NoAudioStream,
    #[error("The selected audio track is not available.")]
    InvalidAudioTrack,
    #[error("REENCODE_APPROVAL_REQUIRED")]
    ConversionApprovalRequired,
    #[error("Could not process the audio file: {0}")]
    Convert(String),
    #[error("A segment could not be reduced below the 20 MB upload limit.")]
    StillTooLarge,
    #[error("File operation failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub duration_seconds: f64,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub container: String,
    pub is_video: bool,
    pub audio_tracks: Vec<AudioTrackInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrackInfo {
    pub stream_index: u32,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub requires_conversion: bool,
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    #[serde(default)]
    tags: ProbeTags,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeTags {
    language: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    format_name: Option<String>,
}

pub async fn inspect(path: &Path) -> Result<AudioInfo, AudioError> {
    if !path.is_file() {
        return Err(AudioError::MissingFile);
    }
    let probe = probe(path).await?;
    let audio_streams: Vec<&ProbeStream> = probe
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .collect();
    let stream = audio_streams.first().ok_or(AudioError::NoAudioStream)?;
    let metadata = tokio::fs::metadata(path).await?;
    let container = probe.format.format_name.clone().unwrap_or_default();
    let is_video = probe
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("video"));
    let audio_tracks = audio_streams
        .iter()
        .map(|stream| {
            let codec = stream.codec_name.clone().unwrap_or_else(|| "audio".into());
            AudioTrackInfo {
                stream_index: stream.index,
                requires_conversion: lossless_output_extension(&codec).is_none(),
                codec,
                sample_rate: parse_sample_rate(stream),
                channels: stream.channels,
                language: stream.tags.language.clone(),
                title: stream.tags.title.clone(),
            }
        })
        .collect();

    Ok(AudioInfo {
        path: path.to_string_lossy().into_owned(),
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        size_bytes: metadata.len(),
        duration_seconds: probe
            .format
            .duration
            .as_deref()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0),
        codec: stream.codec_name.clone().unwrap_or_else(|| "audio".into()),
        sample_rate: parse_sample_rate(stream),
        channels: stream.channels,
        container,
        is_video,
        audio_tracks,
        context_prompt: None,
    })
}

pub async fn prepare(
    source: &Path,
    compress_audio: bool,
    compression_quality: &str,
    audio_stream_index: u32,
    allow_reencode: bool,
    work_dir: &Path,
) -> Result<Vec<PathBuf>, AudioError> {
    let probe = probe(source).await?;
    let audio_streams: Vec<&ProbeStream> = probe
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .collect();
    if audio_streams.is_empty() {
        return Err(AudioError::NoAudioStream);
    }
    let stream = audio_streams
        .iter()
        .find(|stream| stream.index == audio_stream_index)
        .copied()
        .ok_or(AudioError::InvalidAudioTrack)?;
    let codec = stream.codec_name.as_deref().unwrap_or("audio");
    let has_video = probe
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("video"));
    let needs_track_extraction = has_video || audio_streams.len() > 1;

    let prepared = if compress_audio {
        let compressed = work_dir.join("compressed.m4a");
        compress(source, &compressed, audio_stream_index, compression_quality).await?;
        compressed
    } else if let Some(extension) = lossless_output_extension(codec) {
        let container_is_compatible = is_api_compatible_audio_file(
            source,
            probe.format.format_name.as_deref().unwrap_or_default(),
            codec,
        );
        if !needs_track_extraction && container_is_compatible {
            source.to_path_buf()
        } else {
            let extracted = work_dir.join(format!("extracted.{extension}"));
            match extract_without_reencoding(source, &extracted, audio_stream_index).await {
                Ok(()) => extracted,
                Err(AudioError::Convert(_)) if allow_reencode => {
                    let converted = work_dir.join("converted.m4a");
                    compress(source, &converted, audio_stream_index, compression_quality).await?;
                    converted
                }
                Err(AudioError::Convert(_)) => return Err(AudioError::ConversionApprovalRequired),
                Err(error) => return Err(error),
            }
        }
    } else if allow_reencode {
        let converted = work_dir.join("converted.m4a");
        compress(source, &converted, audio_stream_index, compression_quality).await?;
        converted
    } else {
        return Err(AudioError::ConversionApprovalRequired);
    };

    let info = inspect(&prepared).await?;
    if info.size_bytes <= MAX_UPLOAD_BYTES && info.duration_seconds <= TARGET_DURATION_SECONDS {
        return Ok(vec![prepared]);
    }

    split_without_reencoding(&prepared, work_dir).await
}

fn parse_sample_rate(stream: &ProbeStream) -> Option<u32> {
    stream
        .sample_rate
        .as_deref()
        .and_then(|value| value.parse().ok())
}

fn lossless_output_extension(codec: &str) -> Option<&'static str> {
    match codec {
        "aac" | "alac" => Some("m4a"),
        "mp3" => Some("mp3"),
        "opus" | "vorbis" => Some("webm"),
        "flac" => Some("flac"),
        value if value.starts_with("pcm_") => Some("wav"),
        _ => None,
    }
}

pub fn codec_requires_conversion(codec: &str) -> bool {
    lossless_output_extension(codec).is_none()
}

pub async fn extract_local_track(
    source: &Path,
    stream_index: u32,
    output_dir: &Path,
) -> Result<PathBuf, AudioError> {
    let info = inspect(source).await?;
    let track = info
        .audio_tracks
        .iter()
        .find(|track| track.stream_index == stream_index)
        .ok_or(AudioError::InvalidAudioTrack)?;
    let extension =
        lossless_output_extension(&track.codec).ok_or(AudioError::ConversionApprovalRequired)?;
    tokio::fs::create_dir_all(output_dir).await?;
    let date_dir = output_dir.join(crate::storage::utc_date_now());
    tokio::fs::create_dir_all(&date_dir).await?;
    let name = format!(
        "audio_{}.{}",
        crate::storage::timestamp_millis() / 1000,
        extension
    );
    let target = date_dir.join(name);
    let temporary = target.with_extension(format!("{extension}.part"));
    extract_without_reencoding(source, &temporary, stream_index).await?;
    if let Err(error) = tokio::fs::rename(&temporary, &target).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(AudioError::Io(error));
    }
    inspect(&target).await?;
    Ok(target)
}

fn is_api_compatible_audio_file(path: &Path, format_name: &str, codec: &str) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let formats: Vec<&str> = format_name.split(',').collect();
    match codec {
        "aac" | "alac" => {
            matches!(extension.as_str(), "m4a" | "mp4")
                && formats
                    .iter()
                    .any(|name| matches!(*name, "mov" | "mp4" | "m4a"))
        }
        "mp3" => {
            matches!(extension.as_str(), "mp3" | "mpeg" | "mpga")
                && formats.iter().any(|name| matches!(*name, "mp3" | "mpeg"))
        }
        "opus" | "vorbis" => {
            matches!(extension.as_str(), "webm" | "ogg")
                && formats
                    .iter()
                    .any(|name| matches!(*name, "matroska" | "webm" | "ogg"))
        }
        "flac" => extension == "flac" && formats.contains(&"flac"),
        value if value.starts_with("pcm_") => extension == "wav" && formats.contains(&"wav"),
        _ => false,
    }
}

async fn split_without_reencoding(
    source: &Path,
    work_dir: &Path,
) -> Result<Vec<PathBuf>, AudioError> {
    let info = inspect(source).await?;
    let silence_points = detect_silence_points(source).await.unwrap_or_default();
    // Five minutes is the normal target. High-bitrate inputs use a shorter
    // target so each copied segment stays safely below the 20 MB limit.
    let size_limited_seconds =
        info.duration_seconds * SAFE_UPLOAD_BYTES as f64 / info.size_bytes as f64;
    let target_seconds = TARGET_DURATION_SECONDS.min(size_limited_seconds);
    let initial_ranges = build_ranges(info.duration_seconds, target_seconds, &silence_points);
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3");
    let mut pending: VecDeque<(f64, f64)> = initial_ranges.into();
    let mut accepted = Vec::new();
    let mut output_index = 0usize;

    while let Some((start, end)) = pending.pop_front() {
        output_index += 1;
        let output = work_dir.join(format!("part-{output_index:04}.{extension}"));
        copy_segment(source, &output, start, end - start).await?;
        let output_size = tokio::fs::metadata(&output).await?.len();
        if output_size <= MAX_UPLOAD_BYTES {
            accepted.push(output);
            continue;
        }

        tokio::fs::remove_file(&output).await?;
        if end - start < 1.0 {
            return Err(AudioError::StillTooLarge);
        }
        let midpoint = (start + end) / 2.0;
        let split = nearest_silence(midpoint, start, end, &silence_points).unwrap_or(midpoint);
        pending.push_front((split, end));
        pending.push_front((start, split));
    }

    Ok(accepted)
}

async fn probe(path: &Path) -> Result<ProbeOutput, AudioError> {
    let binary = find_binary("ffprobe").ok_or(AudioError::FfmpegMissing)?;
    let output = Command::new(binary)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .await?;
    if !output.status.success() {
        return Err(AudioError::Probe(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| AudioError::Probe(error.to_string()))
}

async fn extract_without_reencoding(
    source: &Path,
    output: &Path,
    audio_stream_index: u32,
) -> Result<(), AudioError> {
    let binary = find_binary("ffmpeg").ok_or(AudioError::FfmpegMissing)?;
    let result = Command::new(binary)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .arg("-map")
        .arg(format!("0:{audio_stream_index}"))
        .args(["-vn", "-map_metadata", "-1", "-c:a", "copy"])
        .arg(output)
        .output()
        .await?;
    if !result.status.success() {
        return Err(AudioError::Convert(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

async fn compress(
    source: &Path,
    output: &Path,
    audio_stream_index: u32,
    quality: &str,
) -> Result<(), AudioError> {
    let (sample_rate, bitrate) = match quality {
        "low" => (16_000, 32),
        "standard" => (16_000, 64),
        "high" => (24_000, 96),
        "best" => (48_000, 128),
        _ => {
            return Err(AudioError::Convert(
                "Choose a supported compact audio quality.".into(),
            ))
        }
    };
    let binary = find_binary("ffmpeg").ok_or(AudioError::FfmpegMissing)?;
    let result = Command::new(binary)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .arg("-map")
        .arg(format!("0:{audio_stream_index}"))
        .args([
            "-vn",
            "-map_metadata",
            "-1",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
        ])
        .args(["-codec:a", "aac", "-b:a", &format!("{bitrate}k")])
        .arg(output)
        .output()
        .await?;
    if !result.status.success() {
        return Err(AudioError::Convert(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

async fn copy_segment(
    source: &Path,
    output: &Path,
    start: f64,
    length: f64,
) -> Result<(), AudioError> {
    let binary = find_binary("ffmpeg").ok_or(AudioError::FfmpegMissing)?;
    let result = Command::new(binary)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            &format!("{start:.3}"),
            "-i",
        ])
        .arg(source)
        .args([
            "-t",
            &format!("{length:.3}"),
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "copy",
        ])
        .arg(output)
        .output()
        .await?;
    if !result.status.success() {
        return Err(AudioError::Convert(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

async fn detect_silence_points(path: &Path) -> Result<Vec<f64>, AudioError> {
    let binary = find_binary("ffmpeg").ok_or(AudioError::FfmpegMissing)?;
    let output = Command::new(binary)
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-af", "silencedetect=noise=-35dB:d=0.5", "-f", "null", "-"])
        .stdout(Stdio::null())
        .output()
        .await?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expression = Regex::new(r"silence_end:\s*([0-9.]+)\s*\|\s*silence_duration:\s*([0-9.]+)")
        .expect("valid silence regex");
    Ok(expression
        .captures_iter(&stderr)
        .filter_map(|capture| {
            let end: f64 = capture.get(1)?.as_str().parse().ok()?;
            let length: f64 = capture.get(2)?.as_str().parse().ok()?;
            Some(end - length / 2.0)
        })
        .collect())
}

fn build_ranges(duration: f64, target_seconds: f64, silences: &[f64]) -> Vec<(f64, f64)> {
    let mut ranges = Vec::new();
    let mut start = 0.0;
    while duration - start > target_seconds {
        let ideal_end = start + target_seconds;
        let earliest = start + target_seconds * 0.8;
        let end = silences
            .iter()
            .copied()
            .rfind(|point| *point >= earliest && *point <= ideal_end)
            .unwrap_or(ideal_end);
        ranges.push((start, end));
        start = end;
    }
    if duration > start {
        ranges.push((start, duration));
    }
    ranges
}

fn nearest_silence(target: f64, start: f64, end: f64, silences: &[f64]) -> Option<f64> {
    let lower = start + (end - start) * 0.25;
    let upper = start + (end - start) * 0.75;
    silences
        .iter()
        .copied()
        .filter(|point| *point >= lower && *point <= upper)
        .min_by(|left, right| (left - target).abs().total_cmp(&(right - target).abs()))
}

pub(crate) fn find_binary(name: &str) -> Option<PathBuf> {
    let override_name = format!("MINEFIA_{}", name.to_uppercase());
    if let Some(path) = env::var_os(override_name)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }

    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(&executable);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
        .iter()
        .map(|directory| Path::new(directory).join(&executable))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_near_silence_without_oversized_ranges() {
        let ranges = build_ranges(130.0, 60.0, &[42.0, 58.0, 99.0, 117.0]);
        assert_eq!(ranges, vec![(0.0, 58.0), (58.0, 117.0), (117.0, 130.0)]);
    }

    #[test]
    fn five_minute_ranges_keep_source_order_and_prefer_silence() {
        let ranges = build_ranges(910.0, 300.0, &[285.0, 580.0, 870.0]);
        assert_eq!(
            ranges,
            vec![(0.0, 285.0), (285.0, 580.0), (580.0, 870.0), (870.0, 910.0)]
        );
    }

    #[test]
    fn chooses_nearest_silence_away_from_edges() {
        assert_eq!(
            nearest_silence(50.0, 0.0, 100.0, &[5.0, 45.0, 70.0, 95.0]),
            Some(45.0)
        );
    }

    #[test]
    fn chooses_lossless_containers_by_codec() {
        assert_eq!(lossless_output_extension("aac"), Some("m4a"));
        assert_eq!(lossless_output_extension("mp3"), Some("mp3"));
        assert_eq!(lossless_output_extension("opus"), Some("webm"));
        assert_eq!(lossless_output_extension("vorbis"), Some("webm"));
        assert_eq!(lossless_output_extension("pcm_s16le"), Some("wav"));
        assert_eq!(lossless_output_extension("ac3"), None);
    }

    #[test]
    fn checks_codec_container_and_extension_together() {
        assert!(is_api_compatible_audio_file(
            Path::new("recording.m4a"),
            "mov,mp4,m4a,3gp,3g2,mj2",
            "aac"
        ));
        assert!(!is_api_compatible_audio_file(
            Path::new("recording.m4a"),
            "matroska,webm",
            "aac"
        ));
        assert!(!is_api_compatible_audio_file(
            Path::new("recording.bin"),
            "mov,mp4,m4a,3gp,3g2,mj2",
            "aac"
        ));
    }
}
