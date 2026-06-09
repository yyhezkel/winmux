//! Phase 58: speech-to-text — local-endpoint backend.
//!
//! The Web Speech API backend lives entirely in the frontend
//! (`window.SpeechRecognition` is part of WebView2 on Windows). This
//! module handles only the Local backend: POST recorded audio bytes
//! to a user-configurable HTTP endpoint and return the transcribed
//! text.
//!
//! Multipart shape mirrors OpenAI's `/v1/audio/transcriptions` so
//! whisper.cpp's server, faster-whisper-server, and OpenAI-compatible
//! local proxies all work without per-server adapters:
//!
//! ```text
//! POST <endpoint> HTTP/1.1
//! Content-Type: multipart/form-data; boundary=<BOUNDARY>
//!
//! --<BOUNDARY>
//! Content-Disposition: form-data; name="file"; filename="audio.webm"
//! Content-Type: audio/webm
//!
//! <raw audio bytes>
//! --<BOUNDARY>
//! Content-Disposition: form-data; name="language"
//!
//! <language>
//! --<BOUNDARY>--
//! ```
//!
//! Expected response: `{ "text": "transcribed string" }` (also OpenAI-
//! compatible). Anything else surfaces a clean error.

use serde::Deserialize;
use tauri::State;

use crate::{dlog, AppState};

#[derive(Deserialize)]
struct TranscribeResponse {
    text: String,
}

/// 30s timeout. The frontend caps recording at ~30s anyway, so a
/// longer ceiling here would just block on a dead endpoint.
const TIMEOUT_SECS: u64 = 30;

/// Build a multipart/form-data body for the audio + language fields.
/// The boundary is a hex hash of the audio length + a process-uptime
/// counter (no Math.random/Date::now disallowed — we use an
/// AtomicU64 monotonic counter to keep tests / resumed workflows
/// deterministic).
fn build_multipart(audio: &[u8], language: &str, boundary: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(audio.len() + 512);
    // file field
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.webm\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/webm\r\n\r\n");
    body.extend_from_slice(audio);
    body.extend_from_slice(b"\r\n");
    // language field
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"language\"\r\n\r\n");
    body.extend_from_slice(language.as_bytes());
    body.extend_from_slice(b"\r\n");
    // closing boundary
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"--\r\n");
    body
}

/// Monotonic counter for boundary uniqueness. Doesn't need to be
/// secret — boundaries only have to be unique within a single
/// request, which is trivially satisfied by appending a counter to a
/// fixed prefix.
static BOUNDARY_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn make_boundary() -> String {
    let n = BOUNDARY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("----winmux-stt-{:016x}", n)
}

#[tauri::command]
pub(crate) async fn stt_transcribe_local(
    state: State<'_, AppState>,
    audio_bytes: Vec<u8>,
    language: String,
) -> Result<String, String> {
    if audio_bytes.is_empty() {
        return Err("stt: empty audio buffer".into());
    }
    // Pull the endpoint out of settings under a brief lock. We
    // deliberately do NOT pass the endpoint as a command arg — that
    // would let any (otherwise-trusted) frontend code POST to an
    // attacker-controlled URL just by Invoking us with a swapped
    // value. Single source of truth = user's settings.json.
    let endpoint = {
        let s = state
            .settings
            .lock()
            .map_err(|e| format!("settings lock: {e}"))?;
        s.stt
            .local_endpoint
            .clone()
            .ok_or_else(|| "stt: no local_endpoint configured in Settings → Voice input".to_string())?
    };
    if endpoint.trim().is_empty() {
        return Err("stt: local_endpoint is empty".into());
    }
    let boundary = make_boundary();
    let body = build_multipart(&audio_bytes, &language, &boundary);
    let content_type = format!("multipart/form-data; boundary={boundary}");

    // ureq is sync; offload to a blocking pool so we don't block the
    // tokio runtime. Same shape as updater::fetch_manifest.
    let endpoint_log = endpoint.clone();
    let audio_len = audio_bytes.len();
    let text = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let resp = ureq::post(&endpoint)
            .set("Content-Type", &content_type)
            .set(
                "User-Agent",
                &format!("winmux/{}", env!("CARGO_PKG_VERSION")),
            )
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .send_bytes(&body)
            .map_err(|e| format!("stt POST: {e}"))?;
        if resp.status() < 200 || resp.status() >= 300 {
            return Err(format!("stt HTTP {}", resp.status()));
        }
        let body = resp
            .into_string()
            .map_err(|e| format!("stt read body: {e}"))?;
        let parsed: TranscribeResponse = serde_json::from_str(body.trim_start_matches('\u{FEFF}'))
            .map_err(|e| format!("stt parse response: {e} (raw: {body})"))?;
        Ok(parsed.text)
    })
    .await
    .map_err(|e| format!("stt join: {e}"))??;

    dlog(&format!(
        "stt_transcribe_local: endpoint={} audio_bytes={} returned_chars={}",
        endpoint_log,
        audio_len,
        text.chars().count()
    ));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_multipart_shape() {
        let body = build_multipart(b"abc", "he-IL", "BOUND");
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("--BOUND\r\nContent-Disposition: form-data; name=\"file\""));
        assert!(s.contains("filename=\"audio.webm\""));
        assert!(s.contains("Content-Type: audio/webm\r\n\r\nabc\r\n"));
        assert!(s.contains("name=\"language\"\r\n\r\nhe-IL\r\n"));
        assert!(s.ends_with("--BOUND--\r\n"));
    }

    #[test]
    fn make_boundary_is_unique_per_call() {
        let a = make_boundary();
        let b = make_boundary();
        assert_ne!(a, b);
        assert!(a.starts_with("----winmux-stt-"));
    }
}
