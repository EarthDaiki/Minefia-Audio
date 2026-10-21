use std::path::PathBuf;
use tauri::Manager;

pub fn recordings_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recordings"))
        .map_err(|error| error.to_string())
}

pub fn dated_recording_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(recordings_root(app)?.join("recorded").join(utc_date_now()))
}

pub fn dated_download_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(recordings_root(app)?
        .join("downloaded")
        .join(utc_date_now()))
}

pub fn extracted_media_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(recordings_root(app)?.join("extracted"))
}

pub fn transcript_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("transcript"))
        .map_err(|error| error.to_string())
}

pub fn utc_date_now() -> String {
    let days = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400) as i64;
    civil_date_from_days(days)
}

/// A filesystem-safe 24-hour UTC time (hour-minute-second) for transcript session folders.
pub fn utc_time_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let day_seconds = now.as_secs() % 86_400;
    let hours = day_seconds / 3_600;
    let minutes = (day_seconds % 3_600) / 60;
    let seconds = day_seconds % 60;
    format!("{hours:02}-{minutes:02}-{seconds:02}")
}

fn civil_date_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::civil_date_from_days;

    #[test]
    fn unix_epoch_dates_are_formatted_as_calendar_days() {
        assert_eq!(civil_date_from_days(0), "1970-01-01");
        assert_eq!(civil_date_from_days(11_016), "2000-02-29");
        assert_eq!(civil_date_from_days(20_000), "2024-10-04");
    }

    #[test]
    fn transcript_time_uses_hour_minute_second() {
        let time = super::utc_time_now();
        assert_eq!(time.len(), 8);
        assert_eq!(time.as_bytes()[2], b'-');
        assert_eq!(time.as_bytes()[5], b'-');
        assert!(time
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 5) || byte.is_ascii_digit()));
    }
}
