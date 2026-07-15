//! beta.3 (netfree): HTTP GET wrapper with jittered exponential backoff.
//!
//! Motivation: users behind flaky links (mobile hotspot, hotel wifi) and
//! MITM filters (Netfree — Israeli ISP-level content filter) hit transient
//! TLS / TCP errors on the updater path. A single failed GET means "Check
//! for updates" surfaces a red toast even when the network recovers 800ms
//! later. Retrying with backoff turns most of those into invisible blips.
//!
//! Design constraints:
//! - GET only. POST is *never* auto-retried — a POST is not idempotent, and
//!   the STT cloud in particular would double-charge / double-log on a
//!   duplicate request that the first attempt actually succeeded in
//!   delivering before the transport dropped.
//! - Only network errors (`ureq::Error::Transport`) get retried. HTTP-status
//!   errors (4xx, 5xx) are surfaced immediately — they represent a bad
//!   request or a genuinely broken server, not a network glitch, and
//!   retrying them just delays the real error.
//! - Backoff: 500ms → 1s → 2s, ±20% jitter, capped at `MAX_ATTEMPTS`.
//! - Rule #1 compliance: never logs URL query strings or bodies. Retry
//!   traces log the host component only.
//!
//! Usage — the caller passes a *closure* that builds a fresh `ureq::Request`
//! each attempt (headers, timeout, etc. re-applied on retry):
//!
//! ```ignore
//! use winmux_core::http::get_with_retry;
//! let resp = get_with_retry(|| {
//!     ureq::get(&url)
//!         .set("User-Agent", ua)
//!         .timeout(std::time::Duration::from_secs(8))
//! })?;
//! ```

use std::time::Duration;

/// How many *total* attempts a `get_with_retry` call will make before giving
/// up (initial try + retries). 3 = one immediate attempt plus two more with
/// backoff — the sweet spot for transient MITM/hotel-wifi glitches without
/// stretching the user-visible "Check for updates" latency past ~4s.
pub const MAX_ATTEMPTS: usize = 3;

/// Base delays applied *before* attempt N (N starts at 1). Index 0 is the
/// wait before the first retry, index 1 before the second retry. If we ever
/// bump `MAX_ATTEMPTS` above 3, extend this table — `attempt_delay` clamps
/// to the last entry so we don't panic.
const BACKOFF: &[Duration] = &[
    Duration::from_millis(500),
    Duration::from_millis(1_000),
    Duration::from_millis(2_000),
];

/// ±20% jitter — spreads out concurrent retries from multiple clients that
/// all failed at the same instant (thundering herd on a filter that's
/// coming back up).
const JITTER_PCT: f64 = 0.20;

/// Retry the GET produced by `mk` up to `MAX_ATTEMPTS` times, with jittered
/// exponential backoff, on transport-level failures only.
///
/// `mk` is called fresh for each attempt so that per-request state
/// (headers, timeout, cookie jar) is applied on every retry — `ureq`
/// consumes the `Request` when you call `.call()`, so we can't reuse it.
///
/// Returns the successful `ureq::Response`, or the *last* error observed
/// if all attempts failed. If the caller ever sees `ureq::Error::Status`
/// come back, it means the server responded (with 4xx/5xx) — those are
/// deliberately not retried; see module doc.
pub fn get_with_retry<F>(mk: F) -> Result<ureq::Response, ureq::Error>
where
    F: Fn() -> ureq::Request,
{
    let mut last_err: Option<ureq::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let d = attempt_delay(attempt - 1);
            std::thread::sleep(d);
        }
        let req = mk();
        // Only pull the host out for logging — never the full URL, per Rule #1.
        let host_for_log = req.url().to_string();
        let host_for_log = redact_host(&host_for_log);
        match req.call() {
            Ok(resp) => {
                if attempt > 0 {
                    crate::dlog_tag(
                        "HTTP",
                        &format!("get_with_retry: recovered on attempt {} host={host_for_log}", attempt + 1),
                    );
                }
                return Ok(resp);
            }
            Err(ureq::Error::Status(code, resp)) => {
                // Server responded — not a network glitch. Do not retry.
                return Err(ureq::Error::Status(code, resp));
            }
            Err(e @ ureq::Error::Transport(_)) => {
                crate::dlog_tag(
                    "HTTP",
                    &format!(
                        "get_with_retry: transport error on attempt {}/{MAX_ATTEMPTS} host={host_for_log}",
                        attempt + 1
                    ),
                );
                last_err = Some(e);
                continue;
            }
        }
    }
    // Every attempt failed with a Transport error. Return the last one so
    // the caller's error message reflects the freshest failure.
    // Structural invariant: MAX_ATTEMPTS >= 1 (compile-time const), the
    // loop always runs at least once, and any Err arm assigns `last_err`
    // before `continue`. So `last_err` MUST be Some here — the None arm
    // is a "cannot happen" assertion (unreachable!, not unwrap — per
    // CLAUDE.md Rule "no unwrap/expect outside main").
    match last_err {
        Some(e) => Err(e),
        None => unreachable!(
            "get_with_retry: last_err was None after {MAX_ATTEMPTS} attempts (impossible)"
        ),
    }
}

/// Compute the sleep before retry number `idx` (0-indexed into `BACKOFF`)
/// applying ±20% jitter. Clamps `idx` so callers that bump `MAX_ATTEMPTS`
/// above the table length don't panic — they get the last entry's delay.
fn attempt_delay(idx: usize) -> Duration {
    let base = BACKOFF.get(idx).copied().unwrap_or_else(|| {
        BACKOFF.last().copied().unwrap_or(Duration::from_millis(500))
    });
    apply_jitter(base)
}

fn apply_jitter(base: Duration) -> Duration {
    let base_ms = base.as_millis() as f64;
    let jitter = base_ms * JITTER_PCT * (pseudo_rand_unit() * 2.0 - 1.0);
    let ms = (base_ms + jitter).max(0.0) as u64;
    Duration::from_millis(ms)
}

/// Cheap 0.0..1.0 pseudo-random without pulling in `rand` into this leaf
/// module. Uses nanosecond time as entropy — good enough for jitter, NOT
/// good enough for anything security-sensitive (don't reuse this).
fn pseudo_rand_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Fold high-order bits into the low ones so the result isn't just a
    // slowly-incrementing counter across two calls in the same microsecond.
    let mixed = n ^ (n.wrapping_mul(2_654_435_761));
    (mixed as f64) / (u32::MAX as f64)
}

/// Rule #1 helper: for logging, keep only "scheme://host" and drop the
/// path/query so log lines can't leak URL parameters (auth tokens, etc.).
fn redact_host(url: &str) -> String {
    // Fast path: split at the third '/' — after "https://host".
    let mut slashes = 0;
    for (i, c) in url.char_indices() {
        if c == '/' {
            slashes += 1;
            if slashes == 3 {
                return url[..i].to_string();
            }
        }
    }
    url.to_string()
}

// ─── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_host_strips_path_and_query() {
        assert_eq!(
            redact_host("https://api.github.com/repos/owner/repo?token=SECRET"),
            "https://api.github.com"
        );
        assert_eq!(
            redact_host("https://example.com/manifest.json"),
            "https://example.com"
        );
    }

    #[test]
    fn redact_host_bare_host_passthrough() {
        // No path at all → return the input unchanged (still fine for logging).
        assert_eq!(redact_host("https://example.com"), "https://example.com");
    }

    #[test]
    fn attempt_delay_falls_within_jitter_window() {
        // idx=0 → base 500ms, ±20% → [400, 600]ms.
        for _ in 0..50 {
            let d = attempt_delay(0);
            let ms = d.as_millis();
            assert!(
                (400..=600).contains(&ms),
                "expected 400..=600ms, got {ms}ms"
            );
        }
    }

    #[test]
    fn attempt_delay_clamps_past_table_end() {
        // idx way past the table end → still returns something (last entry's window).
        // 2000ms ±20% → [1600, 2400]ms.
        let d = attempt_delay(99);
        let ms = d.as_millis();
        assert!(
            (1600..=2400).contains(&ms),
            "expected 1600..=2400ms, got {ms}ms"
        );
    }

    #[test]
    fn max_attempts_is_three() {
        // Bump this test intentionally if we ever change the constant — the
        // number is load-bearing (documented in comments + user-visible retry
        // toast text "attempt N/3").
        assert_eq!(MAX_ATTEMPTS, 3);
    }

    #[test]
    fn pseudo_rand_unit_stays_in_range() {
        for _ in 0..100 {
            let x = pseudo_rand_unit();
            assert!((0.0..=1.0).contains(&x), "out of range: {x}");
        }
    }
}
