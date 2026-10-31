use base64::{engine::general_purpose::STANDARD, Engine};
use headless_chrome::{Browser, FetcherOptions, LaunchOptionsBuilder};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

static BROWSER_PROFILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMediaCandidate {
    pub url: String,
    pub kind: String,
    pub content_type: Option<String>,
    pub duration_seconds: Option<f64>,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMediaInfo {
    pub page_url: String,
    pub page_title: String,
    pub media: Vec<PageMediaCandidate>,
    pub network_media: Vec<PageMediaCandidate>,
    pub master_playlists: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
}

pub struct BrowserInspection {
    pub info: PageMediaInfo,
    pub cookies: Vec<BrowserCookie>,
    pub user_agent: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageScan {
    title: String,
    media: Vec<PageMediaCandidate>,
    has_blob_media: bool,
}

pub fn inspect(
    page_url: String,
    profile_dir: PathBuf,
    chromium_dir: PathBuf,
    headless_mode: bool,
    cancel: &AtomicBool,
) -> Result<PageMediaInfo, String> {
    inspect_impl(
        page_url,
        profile_dir,
        chromium_dir,
        false,
        headless_mode,
        Some(cancel),
    )
    .map(|inspection| inspection.info)
}

pub fn inspect_with_session(
    page_url: String,
    profile_dir: PathBuf,
    chromium_dir: PathBuf,
    headless_mode: bool,
    cancel: &AtomicBool,
) -> Result<BrowserInspection, String> {
    inspect_impl(
        page_url,
        profile_dir,
        chromium_dir,
        true,
        headless_mode,
        Some(cancel),
    )
}

fn inspect_impl(
    page_url: String,
    profile_dir: PathBuf,
    chromium_dir: PathBuf,
    include_session: bool,
    headless_mode: bool,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserInspection, String> {
    let _profile_guard = BROWSER_PROFILE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "The browser profile is currently unavailable.".to_owned())?;
    validate_public_http_url(&page_url)?;
    std::fs::create_dir_all(&profile_dir).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&chromium_dir).map_err(|error| error.to_string())?;

    // Keep discovery order so the first playlist can be offered by default.
    let playlists = Arc::new(Mutex::new(Vec::new()));
    let network_media = Arc::new(Mutex::new(BTreeMap::new()));
    let network_last_activity = Arc::new(Mutex::new(Instant::now()));
    let (browser, tab) = create_browser_tab(
        &profile_dir,
        &chromium_dir,
        headless_mode,
        &playlists,
        &network_media,
        &network_last_activity,
    )?;
    tab.navigate_to(&page_url)
        .map_err(|error| format!("Could not load the page: {error}"))?;
    if is_canceled(cancel) {
        return Err("CANCELED".into());
    }

    wait_for_html_complete(&tab, Duration::from_secs(15), cancel);
    if is_canceled(cancel) {
        return Err("CANCELED".into());
    }
    let (browser, tab) = if headless_mode {
        let redirected_url = tab.get_url();
        if is_login_url(&redirected_url) {
            // Headless inspection detects provider sign-in redirects; reopen
            // the same private profile visibly so the user can authenticate.
            drop(tab);
            drop(browser);
            let (visible_browser, visible_tab) = create_browser_tab(
                &profile_dir,
                &chromium_dir,
                false,
                &playlists,
                &network_media,
                &network_last_activity,
            )?;
            visible_tab
                .navigate_to(&redirected_url)
                .map_err(|error| format!("Could not open the provider sign-in page: {error}"))?;
            wait_for_html_complete(&visible_tab, Duration::from_secs(15), cancel);
            if is_canceled(cancel) {
                return Err("CANCELED".into());
            }
            (visible_browser, visible_tab)
        } else {
            (browser, tab)
        }
    } else {
        (browser, tab)
    };
    // Hold the Browser owner so the app's private browser process stays alive until inspection ends.
    let _browser = browser;

    // First snapshot loaded HTML media elements; only then wait for observed
    // network activity to become idle (or reach its one-minute deadline).
    let initial_scan = scan_page_media(&tab)?;
    if let Ok(mut last_activity) = network_last_activity.lock() {
        *last_activity = Instant::now();
    }
    let wait_timed_out = !wait_for_network_idle(
        &network_last_activity,
        Duration::from_secs(60),
        Duration::from_secs(5),
        cancel,
    )?;
    if is_canceled(cancel) {
        return Err("CANCELED".into());
    }
    let final_scan = scan_page_media(&tab)?;
    let scan = merge_page_scans(initial_scan, final_scan);
    let found = playlists
        .lock()
        .map_err(|_| "Could not read the browser network log.".to_owned())?
        .clone();
    let network_sources = network_media
        .lock()
        .map_err(|_| "Could not read browser media responses.".to_owned())?
        .values()
        .cloned()
        .collect::<Vec<_>>();

    if scan.media.is_empty() && network_sources.is_empty() && found.is_empty() {
        if scan.has_blob_media {
            let timeout_note = if wait_timed_out {
                " The 1-minute network-idle wait expired."
            } else {
                ""
            };
            return Err(format!("MASTER_PLAYLIST_NOT_FOUND: The loaded video uses a blob URL, but no master.m3u8 request appeared in the observed network traffic.{timeout_note}"));
        }
        let timeout_note = if wait_timed_out {
            " The 1-minute network-idle wait expired."
        } else {
            ""
        };
        return Err(format!("WEB_MEDIA_NOT_FOUND: No loaded direct video source or HLS master playlist was found.{timeout_note}"));
    }

    let user_agent = tab
        .evaluate("navigator.userAgent", false)
        .ok()
        .and_then(|result| result.value)
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "Mozilla/5.0".into());
    let cookies = if include_session {
        tab.call_method(headless_chrome::protocol::cdp::Network::GetAllCookies(None))
            .map_err(|error| format!("Could not read the browser session cookies: {error}"))?
            .cookies
            .into_iter()
            .map(|cookie| BrowserCookie {
                name: cookie.name,
                value: cookie.value,
                domain: cookie.domain,
                path: cookie.path,
                secure: cookie.secure,
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(BrowserInspection {
        info: PageMediaInfo {
            page_url,
            page_title: scan.title,
            media: scan.media,
            network_media: network_sources,
            master_playlists: found,
        },
        cookies,
        user_agent,
    })
}

fn create_browser_tab(
    profile_dir: &Path,
    chromium_dir: &Path,
    headless: bool,
    playlists: &Arc<Mutex<Vec<String>>>,
    network_media: &Arc<Mutex<BTreeMap<String, PageMediaCandidate>>>,
    network_last_activity: &Arc<Mutex<Instant>>,
) -> Result<(Browser, Arc<headless_chrome::Tab>), String> {
    let chrome_path = find_google_chrome();
    let options = LaunchOptionsBuilder::default()
        .headless(headless)
        .sandbox(true)
        .user_data_dir(Some(profile_dir.to_path_buf()))
        // Prefer branded Chrome when present so site players see its full media-codec support.
        // The app-managed Chromium fetcher remains the self-contained fallback.
        .path(chrome_path)
        .fetcher_options(
            FetcherOptions::default()
                .with_install_dir(Some(chromium_dir.to_path_buf()))
                .with_allow_standard_dirs(false),
        )
        .build()
        .map_err(|error| format!("Could not configure the browser: {error}"))?;
    let browser = Browser::new(options)
        .map_err(|error| format!("Could not start the app's private browser: {error}"))?;
    let tab = browser
        .new_tab()
        .map_err(|error| format!("Could not create a browser tab: {error}"))?;

    // Intercept all HTTP requests so pages cannot use this browser to reach private networks.
    tab.enable_request_interception(Arc::new(
        |_transport,
         _session,
         event: headless_chrome::protocol::cdp::Fetch::events::RequestPausedEvent| {
            let url = &event.params.request.url;
            if is_public_http_url(url) {
                headless_chrome::browser::tab::RequestPausedDecision::Continue(None)
            } else {
                headless_chrome::browser::tab::RequestPausedDecision::Fail(
                    headless_chrome::protocol::cdp::Fetch::FailRequest {
                        request_id: event.params.request_id,
                        error_reason:
                            headless_chrome::protocol::cdp::Network::ErrorReason::BlockedByClient,
                    },
                )
            }
        },
    ))
    .map_err(|error| error.to_string())?;
    tab.enable_fetch(None, None)
        .map_err(|error| format!("Could not monitor browser network requests: {error}"))?;

    let captured_playlists = Arc::clone(playlists);
    let captured_network_media = Arc::clone(network_media);
    let captured_network_activity = Arc::clone(network_last_activity);
    tab.register_response_handling(
        "minefia-hls-discovery",
        Box::new(move |params, response_body| {
            if let Ok(mut last_activity) = captured_network_activity.lock() {
                *last_activity = Instant::now();
            }
            let url = params.response.url.clone();
            let mime_type = params.response.mime_type.trim().to_ascii_lowercase();
            let is_media_content =
                mime_type.starts_with("audio/") || mime_type.starts_with("video/");
            if is_media_content && !is_hls_url(&url) && params.response.status < 400 {
                if let Ok(mut sources) = captured_network_media.lock() {
                    if sources.contains_key(&url) || sources.len() < 40 {
                        let kind = if mime_type.starts_with("audio/") {
                            "audio"
                        } else {
                            "video"
                        };
                        sources
                            .entry(url.clone())
                            .or_insert_with(|| PageMediaCandidate {
                                url: url.clone(),
                                kind: kind.to_owned(),
                                content_type: Some(mime_type.clone()),
                                duration_seconds: None,
                                label: format!(
                                    "Network response · {mime_type} · {}",
                                    safe_media_path(&url)
                                ),
                            });
                    }
                }
            }
            if is_hls_url(&url) {
                if let Ok(body) = response_body() {
                    let content = if body.base_64_encoded {
                        STANDARD
                            .decode(body.body)
                            .ok()
                            .and_then(|value| String::from_utf8(value).ok())
                    } else {
                        Some(body.body)
                    };
                    if content.as_deref().is_some_and(is_master_manifest) {
                        if let Ok(mut found) = captured_playlists.lock() {
                            if !found.contains(&url) {
                                found.push(url);
                            }
                        }
                    }
                }
            }
        }),
    )
    .map_err(|error| format!("Could not monitor media responses: {error}"))?;

    Ok((browser, tab))
}

fn scan_page_media(tab: &headless_chrome::Tab) -> Result<PageScan, String> {
    let scan_json = tab
        .evaluate(
            r#"JSON.stringify((() => {
              const elements = [...document.querySelectorAll('video, audio')];
              const media = elements.map((element, index) => {
                const source = element.querySelector('source');
                const url = element.currentSrc || element.src || source?.src || '';
                return {
                  url,
                  kind: element.tagName.toLowerCase(),
                  contentType: source?.type || null,
                  durationSeconds: Number.isFinite(element.duration) ? element.duration : null,
                  label: element.getAttribute('aria-label') || element.getAttribute('title') || `${element.tagName === 'VIDEO' ? 'Video' : 'Audio'} ${index + 1}`
                };
              });
              return {
                title: document.querySelector('title')?.textContent?.trim() || document.title || location.hostname,
                media: media.filter(item => item.url && !item.url.startsWith('blob:') && /^https?:/i.test(item.url)),
                hasBlobMedia: elements.some(element => (element.currentSrc || element.src || '').startsWith('blob:'))
              };
            })())"#,
            false,
        )
        .map_err(|error| format!("Could not inspect page video elements: {error}"))?;
    let json = scan_json
        .value
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| "Could not read video information from the page.".to_owned())?;
    serde_json::from_str(&json)
        .map_err(|error| format!("Could not parse page video information: {error}"))
}

fn merge_page_scans(mut first: PageScan, later: PageScan) -> PageScan {
    let mut seen = first
        .media
        .iter()
        .map(|item| item.url.clone())
        .collect::<BTreeSet<_>>();
    let PageScan {
        title,
        media,
        has_blob_media,
    } = later;
    first.media.extend(
        media
            .into_iter()
            .filter(|item| seen.insert(item.url.clone())),
    );
    if !title.trim().is_empty() {
        first.title = title;
    }
    first.has_blob_media |= has_blob_media;
    first
}

/// Treat the configured quiet response window as Network Idle, with a hard timeout.
/// The timeout is intentional because media pages may continuously stream.
fn wait_for_network_idle(
    last_activity: &Mutex<Instant>,
    timeout: Duration,
    idle_window: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if is_canceled(cancel) {
            return Ok(false);
        }
        let quiet_for = Instant::now().saturating_duration_since(
            *last_activity
                .lock()
                .map_err(|_| "Could not read browser network activity.".to_owned())?,
        );
        if quiet_for >= idle_window {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn find_google_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut candidates = vec![PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        )];
        if let Some(home) = env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            );
        }
        return candidates.into_iter().find(|path| path.is_file());
    }

    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(root) = env::var_os(variable) {
                candidates.push(PathBuf::from(root).join("Google/Chrome/Application/chrome.exe"));
            }
        }
        return candidates.into_iter().find(|path| path.is_file());
    }

    #[cfg(target_os = "linux")]
    {
        let mut candidates = vec![
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/opt/google/chrome/chrome"),
        ];
        if let Some(path) = env::var_os("PATH") {
            for directory in env::split_paths(&path) {
                for executable in ["google-chrome", "google-chrome-stable", "chrome"] {
                    candidates.push(directory.join(executable));
                }
            }
        }
        return candidates
            .into_iter()
            .find(|path| path.is_file() && is_executable(path));
    }

    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "linux")]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn wait_for_html_complete(
    tab: &headless_chrome::Tab,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_canceled(cancel) {
            return;
        }
        let complete = tab
            .evaluate("document.readyState === 'complete'", false)
            .ok()
            .and_then(|result| result.value)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if complete {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn is_canceled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Acquire))
}

fn is_login_url(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        url.host_str().is_some_and(|host| {
            host.split('.')
                .any(|label| label.to_ascii_lowercase().contains("login"))
        }) || url
            .path_segments()
            .into_iter()
            .flatten()
            .any(|part| part.to_ascii_lowercase().contains("login"))
    })
}

fn validate_public_http_url(value: &str) -> Result<(), String> {
    if is_public_http_url(value) {
        Ok(())
    } else {
        Err("Enter an accessible public HTTP or HTTPS page URL.".into())
    }
}

fn is_public_http_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return crate::online_media::is_public_ip(ip);
    }
    (host, port)
        .to_socket_addrs()
        .map(|addresses| {
            let addresses = addresses.collect::<Vec<_>>();
            !addresses.is_empty()
                && addresses
                    .iter()
                    .all(|address| crate::online_media::is_public_ip(address.ip()))
        })
        .unwrap_or(false)
}

fn is_hls_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .is_some_and(|url| url.path().to_ascii_lowercase().ends_with(".m3u8"))
}

fn safe_media_path(value: &str) -> String {
    Url::parse(value)
        .ok()
        .map(|url| {
            url.path_segments()
                .and_then(|segments| segments.filter(|segment| !segment.is_empty()).last())
                .unwrap_or("media file")
                .chars()
                .take(60)
                .collect()
        })
        .unwrap_or_else(|| "media file".into())
}

#[cfg(test)]
mod tests {
    use super::is_login_url;

    #[test]
    fn identifies_login_hosts_and_paths_only_after_page_load_target_is_known() {
        assert!(is_login_url("https://login.nicovideo.jp/login"));
        assert!(is_login_url("https://www.nicovideo.jp/account/login"));
        assert!(!is_login_url(
            "https://www.nicovideo.jp/watch/sm123?next=login"
        ));
    }
}

fn is_master_manifest(value: &str) -> bool {
    let content = value.trim_start();
    content.starts_with("#EXTM3U")
        && (content.contains("#EXT-X-STREAM-INF:") || content.contains("#EXT-X-MEDIA:TYPE=AUDIO"))
}
