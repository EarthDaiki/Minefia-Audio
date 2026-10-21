use serde::Serialize;
use std::fmt::Display;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}

pub fn classify(message: impl Display) -> AppError {
    let message = message.to_string();
    let lower = message.to_ascii_lowercase();
    let code = if message.contains("NO_AUDIO_STREAM") || message.contains("NO_AUDIO") {
        "NO_AUDIO_STREAM"
    } else if message.contains("REENCODE_APPROVAL_REQUIRED") {
        "CONVERSION_REQUIRED"
    } else if lower.contains("conversion was canceled") || lower.contains("conversion cancelled") {
        "CONVERSION_CANCELLED"
    } else if lower.contains("canceled by the user") || lower.contains("cancelled by the user") {
        "OPERATION_CANCELLED"
    } else if message.contains("UNSUPPORTED_CODEC") || lower.contains("unsupported codec") {
        "UNSUPPORTED_CODEC"
    } else if lower.contains("permission")
        || lower.contains("not authorized")
        || lower.contains("access denied")
    {
        "CAPTURE_PERMISSION_DENIED"
    } else if lower.contains("disconnected") || lower.contains("no longer connected") {
        "DEVICE_DISCONNECTED"
    } else if lower.contains("buffer limit")
        || lower.contains("memory limit")
        || lower.contains("upload limit")
    {
        "AUDIO_BUFFER_LIMIT_REACHED"
    } else if lower.contains("429") || lower.contains("rate limit") {
        "API_RATE_LIMITED"
    } else if lower.contains("network")
        || lower.contains("connection")
        || lower.contains("connect")
        || lower.contains("timeout")
    {
        "NETWORK_DISCONNECTED"
    } else if lower.contains("download") {
        "DOWNLOAD_FAILED"
    } else if lower.contains("playlist") || lower.contains("stream not found") {
        "STREAM_NOT_FOUND"
    } else {
        "API_REQUEST_FAILED"
    };
    AppError { code, message }
}
