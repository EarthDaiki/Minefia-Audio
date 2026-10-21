# Minefia Audio

A Tauri desktop app that sends audio to the OpenAI Audio Transcriptions API, displays streamed transcript text as it arrives, and exports the completed transcript.

> **Prototype status:** Minefia Audio is still a prototype and may contain bugs, incomplete workflows, and platform-specific limitations. Do not rely on it as the only copy of important recordings or transcripts; keep backups of source media. Behavior and compatibility may change as the app develops.

## Features

- Live transcript rendering with `stream=true`
- Multiple OpenAI API keys stored individually in the system Keychain
- API key validation with `GET /v1/models` before anything is saved
- Selectable transcription models with short in-app explanations
- Pre-transcription USD cost estimates based on audio duration and the official per-minute model rates
- Estimated upload part count based on file size and compression state
- Local audio and video inspection through `ffprobe` (including MP4, MOV, WebM, MKV, and other FFmpeg-readable formats)
- Selectable audio tracks when a file contains multiple audio streams
- Lossless audio extraction with `-c:a copy` into a codec-compatible API container whenever possible
- Files without an audio stream return `NO_AUDIO_STREAM`
- Re-encoding required by an unsupported codec is blocked until the user confirms the conversion dialog
- Original audio is preserved by default
- Optional **AAC / 64 kbps / 16 kHz / mono** compression
- Audio is split into roughly five-minute parts at silence, with a 20 MB maximum per part
- Up to two parts are transcribed concurrently and merged in source order
- Network timeouts retry up to three times with a short exponential delay
- Split points prefer silence at **-35 dB or quieter for at least 0.5 seconds**
- Oversized variable-bitrate segments are checked and split again when necessary
- Download is enabled only after every audio segment has finished
- Transcript translation and summary translation are optional and shown with their language controls when their source text is available
- Summary, transcript translation, summary translation, and transcript Q&A each have an independently selectable text model in Settings
- The main screen shows the configured text model and an estimate without exposing model pickers there; audio transcription model selection remains in the main workflow
- Transcript and summary sections can be collapsed independently
- Transcript Q&A can use both the current transcript and its generated summary as context
- Completed transcripts can be translated with `stream=true` into Japanese, English, and other common languages
- **GPT-6 Luna** is the default translation model (`reasoning.effort: none`) for fast, economical work
- **GPT-6.1 Sol** is an optional quality-focused translation model (`reasoning.effort: low`)
- Translation models are shown only when returned by `GET /v1/models` for the active API key
- A pre-translation USD estimate shows approximate input/output tokens and model rates
- Long completed transcripts are grouped into expandable, readable sections
- Download confirmation explicitly selects the original or translated transcript
- A unified source picker exposes Local File, System Audio, Microphone, YouTube / URL, HLS Stream, and Web page
- Maximum transcription concurrency is configurable from 1–4 requests (default 2); the same setting applies to file and Standard Mode queues
- File transcription jobs checkpoint prepared chunks and completed transcripts in app data; interrupted jobs can resume without re-uploading completed chunks
- Temporary network failures and HTTP 429/5xx responses retry with exponential backoff and honor `Retry-After` when provided
- Runtime errors carry stable codes such as `NO_AUDIO_STREAM`, `CONVERSION_REQUIRED`, `API_RATE_LIMITED`, and `DEVICE_DISCONNECTED`
- A Markdown summary mode incrementally merges each new transcript chunk into running bullet notes; when recording stops, it creates a final summary from the full transcript
- Summary calls stream from the Responses API and use the selected Luna/Sol text model; the estimated summary cost is shown before enabling the extra processing
- **System Audio · Standard Mode** captures playback audio with ScreenCaptureKit on macOS and WASAPI Loopback on Windows
- Standard Mode waits until 90 seconds, then splits on at least 0.5 seconds of audio at or below -35 dBFS
- A 150-second hard limit guarantees bounded chunks and reports a force-split warning when no silence is available
- Capture continues while up to two transcription requests run concurrently; results are restored to chunk order
- Stopping flushes and transcribes the remaining buffer even when it is shorter than 90 seconds
- Standard Mode records 48 kHz stereo PCM, limits in-memory audio to 64 MB, and reuses the existing 20 MB upload preparation guard
- **Microphone Standard Mode** accepts the selected built-in, USB, headset, or interface input and reuses the exact same 90-second silence / 150-second forced-split pipeline
- Microphone input selection, connection availability, and live input level are shown before and during capture
- **System Audio and Microphone · Live Mode** stream 24 kHz mono PCM to a `gpt-live-transcribe` Realtime transcription session and render partial/final captions as events arrive
- Live Mode uses client-side speech-end detection to commit turns; it does not use Standard Mode's chunk splitter
- Live Mode reconnects after temporary network interruptions and displays elapsed time and an estimated cost at about $1.02/hour
- The live model appears only when the selected key returns `gpt-live-transcribe` from `GET /v1/models`
- YouTube, direct audio/video URLs, and HLS master/index playlists can be inspected before extraction; sources without a confirmed audio stream are rejected
- Web page inspection uses `headless_chrome` with an app-private persistent Chromium profile; it lists currently loaded non-blob `<video>` and `<audio>` sources and monitors completed network responses for actual HLS master playlists
- When a page exposes only blob-backed media, a discovered master playlist is sent through the existing HLS inspection flow; absence of a master playlist is reported as an explicit error
- Chromium is downloaded on first Web page inspection into app data; private IP/local-network browser requests are blocked, and cookies are isolated from the user's regular browser
- Local files can be previewed and transcribed by reference without copying them; local videos retain the original reference while lossless extracted audio is stored in app storage
- YouTube format selection prefers audio-only streams and source audio at or above 64 kbps / 24 kHz when offered; muxed video formats are ranked toward lower resolution
- HLS playlists are inspected for audio groups and referenced variants before extraction; no DRM, sign-in wall, paywall, or other access control is bypassed
- Original audio codecs are copied when supported; any required re-encode is gated by an explicit confirmation dialog
- Online audio is stored in app data and then uses the normal local-file inspection, estimate, transcription, translation, and export workflow; temporary downloaded video is removed unless **Keep downloaded video** is selected
- Newly captured system and microphone audio is saved as 48 kHz stereo PCM WAV files under the app's private data directory (`recordings`), including while Live Mode is streaming; large recordings rotate into multiple files
- Captured audio and extracted online audio are retained; the app does not automatically delete recordings

## Requirements

- Node.js 20 or later
- Stable Rust toolchain
- FFmpeg (`ffmpeg` and `ffprobe`)
- `yt-dlp` for YouTube support; `reqwest` downloads authorized direct/HLS media and FFmpeg is used for media probing, compatible audio extraction, and any required local conversion
- Chromium is automatically installed into the app's private data directory on first Web page inspection
- An OpenAI API key
- macOS 13 or later, or Windows with WASAPI support, for System Audio mode

Install FFmpeg on macOS with:

```bash
brew install ffmpeg
```

Install `yt-dlp` on macOS with `brew install yt-dlp`. YouTube changes frequently, so keep `yt-dlp` current (`brew upgrade yt-dlp`) if extraction fails. Minefia Audio reports an update hint when its YouTube extractor cannot parse a source. Minefia Audio does not bypass source access controls.

If FFmpeg is outside the normal `PATH`, set `MINEFIA_FFMPEG` and `MINEFIA_FFPROBE` to the absolute binary paths.

## Development

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Production users also need FFmpeg. For a self-contained distribution, bundle an FFmpeg binary for each target platform as a Tauri sidecar and review the relevant FFmpeg licensing terms.

On macOS, System Audio uses ScreenCaptureKit and requires access under **System Settings → Privacy & Security → Screen & System Audio Recording**. Microphone capture requires microphone access. The app bundle includes `NSScreenCaptureUsageDescription` and `NSMicrophoneUsageDescription`; restart the app after granting permission if macOS requests it.

## Architecture

- `src/` — React UI and streamed Tauri event handling
- `src-tauri/src/audio.rs` — media inspection, track selection, lossless extraction, optional conversion, silence detection, and lossless splitting
- `src-tauri/src/credentials.rs` — multi-key Keychain storage and non-secret metadata registry
- `src-tauri/src/transcription.rs` — key validation, OpenAI requests, SSE parsing, and transcript assembly
- `src-tauri/src/translation.rs` — safe transcript chunking and translation through the Responses API
- `src-tauri/src/summarization.rs` — cumulative Markdown notes and full-transcript summary through streamed Responses API calls
- `src-tauri/src/pricing.rs` — Standard and Live duration-based transcription estimates used by capture queues
- `src-tauri/src/errors.rs` — stable error-code classification returned to the UI
- `src-tauri/src/system_audio.rs` — native system-audio capture, app-data WAV recording, streaming silence detection, safety limits, and ordered two-request transcription queue
- `src-tauri/src/realtime.rs` — Realtime transcription sessions, PCM resampling, client-side turn commits, reconnect handling, and ordered live captions
- `src-tauri/src/online_media.rs` — YouTube/HLS/direct URL inspection, authorized stream selection, temporary download, lossless audio extraction, and app-data storage
- `src-tauri/src/library.rs` — listing and safely deleting app-managed audio/video files
- `src-tauri/src/storage.rs` — shared app-storage paths, date folders, and timestamp naming
- `src-tauri/src/browser_media.rs` — private headless Chromium profile, loaded `<video>` source discovery, HLS master network monitoring, and private-network request blocking
- `src-tauri/src/lib.rs` — Tauri command boundary

API key secrets never enter the frontend. The Rust backend reads only the selected key from the system Keychain when transcription or translation starts.

App-managed media is grouped by origin below the app-data `recordings` directory: microphone/system captures are in `recordings/recorded/YYYY-MM-DD`, online media audio is in `recordings/downloaded/YYYY-MM-DD`, and audio extracted from a local video is in `recordings/extracted/YYYY-MM-DD`. Existing files in the previous date-only layout remain where they are; the app does not move or remove them. Local audio/video files are stored as path references, not copied; missing references are automatically omitted from the Library. Confirmed deletion of a referenced item deletes the original source file. Transcripts and Markdown summaries are stored under the sibling `transcript/YYYY-MM-DD` directory and appear as separate Library items; summaries update as running notes arrive and are replaced by the final full-transcript summary when processing completes. The Library control in the app header lists media and transcripts from every source and supports preview/open and confirmed deletion. YouTube's temporary download workspace is in app cache and is cleaned after extraction; optionally retained video is stored below that day's downloaded-media directory. App-managed audio and exported transcript filenames use short timestamp-based names.

Technical logs are stored one file per task under the app-data `logs` directory. Routine progress is summarized instead of logging every polling event, while failures retain their error code and full diagnostic message. Log files have no size limit; when a new task of the same type starts, its previous task log is removed and a fresh per-task log is created.

Web page inspection keeps its own persistent Chromium profile at `browser-profile` and its downloaded Chromium runtime at `browser-runtime` in app data. The profile is not shared with Safari/Chrome/other browser profiles. The headless page inspector does not import browser cookies or bypass sign-in/DRM restrictions.

Incomplete local transcription jobs are checkpointed under `transcription-jobs` so they can resume after a transient failure or app restart. Completed API chunks are recorded in the checkpoint and are not uploaded again. App-data recordings and checkpoints are not automatically deleted; saved media and transcripts can be removed from Library.

Transcription estimates use the duration-based rates published on the [OpenAI API pricing page](https://developers.openai.com/api/docs/pricing). Translation estimates use the short-context Standard text-token rates listed there: GPT-6 Luna at $0.10 input / $0.50 output per 1M tokens, and GPT-6.1 Sol at $2.00 input / $10.00 output per 1M tokens. Translation token counts are approximated from transcript characters before the request, so both estimates are planning aids rather than billing guarantees.

The translation request uses the Responses API parameter `reasoning.effort`. Luna supports `none`; GPT-6.1 Sol does not, so Minefia Audio uses Sol's lowest supported setting, `low`.
