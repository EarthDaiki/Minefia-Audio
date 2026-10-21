pub const LIVE_TRANSCRIPTION_PER_MINUTE: f64 = 0.017;

pub fn standard_transcription_per_minute(model: &str) -> Option<f64> {
    match model {
        "gpt-transcribe" => Some(0.0045),
        "gpt-4o-transcribe" => Some(0.006),
        "gpt-4o-mini-transcribe" => Some(0.003),
        // Token-priced; this sentinel allows Standard Mode while the UI omits
        // a misleading per-minute estimate for this model.
        "gpt-4o-transcribe-diarize" => Some(0.0),
        _ => None,
    }
}
