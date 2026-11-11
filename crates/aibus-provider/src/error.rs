//! Provider and config errors (02 section 6). Messages are already redacted.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderErrorKind {
    Auth,
    RateLimit,
    Overloaded,
    ContextTooLong,
    InvalidRequest,
    ModelNotFound,
    Network,
    Timeout,
    Server,
    Cancelled,
    /// A spent allowance or billing cap. Waiting does not help; the account has to.
    Quota,
    /// The provider refused the content. Not an auth problem and not worth a retry.
    ContentFilter,
    Unknown,
}

/// What the UI acts on. `source` is skipped by serde, so a scripted error loaded from RON
/// never carries one.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub retryable: bool,
    #[serde(default)]
    pub retry_after: Option<Duration>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// Already redacted.
    pub message: String,
    /// What the answer's headers said about the account's windows, when it said anything.
    #[serde(default)]
    pub rate_limit: Option<RateLimitInfo>,
    #[serde(skip)]
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ProviderError {
    /// `retryable` follows the kind: transient failures retry, the rest do not.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        use ProviderErrorKind::*;
        let retryable = matches!(kind, RateLimit | Overloaded | Network | Timeout | Server);
        Self {
            kind,
            retryable,
            retry_after: None,
            request_id: None,
            message: message.into(),
            rate_limit: None,
            source: None,
        }
    }
}

/// What a response's headers say about the account's rate-limit windows. Every field is
/// `None` when the provider did not say.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub retry_after: Option<Duration>,
    pub requests_remaining: Option<u64>,
    pub tokens_remaining: Option<u64>,
    /// How long after the response the window that ran out resets. A span from receipt rather
    /// than a wall-clock time, so the limiter's paused-clock tests can drive it.
    pub resets_in: Option<Duration>,
}

/// Seconds since the epoch of a civil date and time (proleptic Gregorian, UTC).
fn epoch_secs(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss
}

fn at_epoch(secs: i64) -> Option<SystemTime> {
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// An RFC 3339 timestamp (`2026-11-11T10:00:00Z`, optional fraction, `Z` or `+hh:mm`), as
/// Anthropic sends its `*-reset` headers.
pub fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    let (date, time) = value.trim().split_once(['T', 't'])?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(i) => (&time[..i], &time[i..]),
        None => (time, "Z"),
    };
    let mut c = clock.splitn(3, ':');
    let hh: i64 = c.next()?.parse().ok()?;
    let mm: i64 = c.next()?.parse().ok()?;
    let secs: f64 = c.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || !(0.0..61.0).contains(&secs) {
        return None;
    }
    let off = match offset.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign => {
            let (h, m) = offset[1..].split_once(':')?;
            let n = h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60;
            if *sign == b'-' { -n } else { n }
        }
    };
    let whole = epoch_secs(y, m, day, hh, mm, secs.trunc() as i64) - off;
    at_epoch(whole).map(|t| t + Duration::from_secs_f64(secs.fract()))
}

/// A `Retry-After` value: seconds, or an HTTP-date (`Wed, 21 Oct 2026 07:28:00 GMT`) counted
/// from `now`. A date already past is zero. Capped at an hour.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let secs = match value.parse::<f64>() {
        Ok(s) => s,
        Err(_) => {
            // IMF-fixdate: day-name, "DD Mon YYYY HH:MM:SS GMT".
            let rest = value.split_once(", ")?.1.strip_suffix(" GMT")?;
            let mut p = rest.split([' ', ':']);
            let day: i64 = p.next()?.parse().ok()?;
            let mon = p.next()?;
            let year: i64 = p.next()?.parse().ok()?;
            let (hh, mm, ss): (i64, i64, i64) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
            let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
                .iter()
                .position(|m| *m == mon)? as i64
                + 1;
            let then = epoch_secs(year, month, day, hh, mm, ss);
            let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            (then - now).max(0) as f64
        }
    };
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs.min(3600.0)))
}

impl ProviderError {
    /// The error for a non-2xx answer, by body code first and status second (04 section 2; the
    /// table is asserted against each provider's recorded error bodies). `message` is what the
    /// provider said, shortened; `code` is the body's `error.code` or `error.type`, or empty.
    pub fn from_http(
        status: u16,
        message: &str,
        retry_after: Option<Duration>,
        request_id: Option<String>,
        code: &str,
    ) -> Self {
        use ProviderErrorKind::*;
        let lower = message.to_ascii_lowercase();
        let code = code.to_ascii_lowercase();
        // The body's own code outranks the status: a spent quota arrives as a 429 and a
        // content filter as a 400, and neither is what its status alone says.
        let quota = status == 402
            || ["insufficient_quota", "usage_limit_reached", "billing_error", "billing"].contains(&code.as_str())
            || (status == 400 && lower.contains("credit balance is too low"));
        let filtered = code.contains("content_filter") || code.contains("content_policy");
        let context = [
            "context_length_exceeded",
            "maximum context length",
            "context window",
            "token limit",
            "prompt is too long",
            "input is too long",
            // Gemini: "The input token count (N) exceeds the maximum number of tokens allowed (M)".
            "exceeds the maximum number of tokens",
        ]
        .iter()
        .any(|p| lower.contains(p));
        let kind = match status {
            _ if quota => Quota,
            _ if filtered => ContentFilter,
            400 | 422 if context => ContextTooLong,
            // Google answers a bad key with a 400, not a 401.
            400 if lower.contains("api key not valid") => Auth,
            400 | 422 => InvalidRequest,
            401 | 403 => Auth,
            404 => ModelNotFound,
            408 | 504 => Timeout,
            413 => ContextTooLong,
            429 => RateLimit,
            503 | 529 => Overloaded,
            500..=599 => Server,
            _ => Unknown,
        };
        let mut message: String = message.chars().take(500).collect();
        if message.is_empty() {
            message = format!("HTTP {status}");
        }
        let mut e = Self::new(kind, message);
        e.retry_after = retry_after;
        e.request_id = request_id;
        e
    }
}

/// A factory could not build a provider from its config, or the registry rejected it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("provider {0}: {1}")]
    Invalid(String, String),
    /// The provider needs a setting no account has given it, which the message names.
    #[error("provider {0}: {1}")]
    NotConfigured(String, String),
    #[error("no provider registered as {0}")]
    UnknownProvider(String),
    #[error("provider {0} registered twice")]
    DuplicateProvider(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(status: u16, message: &str) -> (ProviderErrorKind, bool) {
        let e = ProviderError::from_http(status, message, None, None, "");
        (e.kind, e.retryable)
    }

    #[test]
    fn statuses_map_to_kinds_and_only_transient_ones_retry() {
        use ProviderErrorKind::*;
        assert_eq!(kind(401, "x"), (Auth, false));
        assert_eq!(kind(403, "x"), (Auth, false));
        assert_eq!(kind(404, "x"), (ModelNotFound, false));
        assert_eq!(kind(429, "x"), (RateLimit, true));
        assert_eq!(kind(529, "x"), (Overloaded, true));
        assert_eq!(kind(503, "x"), (Overloaded, true));
        assert_eq!(kind(500, "x"), (Server, true));
        assert_eq!(kind(502, "x"), (Server, true));
        assert_eq!(kind(504, "x"), (Timeout, true));
        assert_eq!(kind(400, "bad field"), (InvalidRequest, false));
        assert_eq!(kind(413, "x"), (ContextTooLong, false));
        assert_eq!(kind(418, "x"), (Unknown, false));
    }

    #[test]
    fn a_400_about_the_context_window_is_context_too_long() {
        for m in [
            "prompt is too long: 213482 tokens > 200000 maximum",
            "This model's maximum context length is 128000 tokens",
            "Your request exceeded model token limit: 8192",
            "The input token count (1196742) exceeds the maximum number of tokens allowed (1048575).",
            "context_length_exceeded",
        ] {
            assert_eq!(kind(400, m), (ProviderErrorKind::ContextTooLong, false), "{m}");
        }
    }

    #[test]
    fn the_message_is_cut_and_never_empty_and_retry_after_parses_seconds() {
        let long = "x".repeat(2_000);
        assert_eq!(ProviderError::from_http(400, &long, None, None, "").message.len(), 500);
        assert_eq!(ProviderError::from_http(500, "", None, None, "").message, "HTTP 500");
        let now = UNIX_EPOCH;
        assert_eq!(parse_retry_after(" 12 ", now), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("1.5", now), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("-3", now), None);
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("999999", now), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn retry_after_takes_the_http_date_form_counted_from_now() {
        // 2026-10-21 07:28:00 UTC is 1_792_567_680 seconds after the epoch.
        let date = "Wed, 21 Oct 2026 07:28:00 GMT";
        let at = UNIX_EPOCH + Duration::from_secs(1_792_567_680);
        assert_eq!(parse_retry_after(date, at - Duration::from_secs(90)), Some(Duration::from_secs(90)));
        assert_eq!(parse_retry_after(date, at + Duration::from_secs(5)), Some(Duration::ZERO), "a past date is zero");
        assert_eq!(parse_retry_after(date, at - Duration::from_secs(86_400)), Some(Duration::from_secs(3600)));
        assert_eq!(parse_retry_after("Wed, 21 Foo 2026 07:28:00 GMT", at), None);
    }

    #[test]
    fn rfc3339_timestamps_parse_with_fraction_and_offset() {
        let t = |s| parse_rfc3339(s).unwrap().duration_since(UNIX_EPOCH).unwrap();
        assert_eq!(t("2026-10-21T07:28:00Z"), Duration::from_secs(1_792_567_680));
        assert_eq!(t("2026-10-21T07:28:00.500Z"), Duration::from_millis(1_792_567_680_500));
        assert_eq!(t("2026-10-21T09:28:00+02:00"), Duration::from_secs(1_792_567_680));
        assert_eq!(parse_rfc3339("yesterday"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_body_code_outranks_the_status() {
        use ProviderErrorKind::*;
        let by_code = |status, code: &str| {
            let e = ProviderError::from_http(status, "x", None, None, code);
            (e.kind, e.retryable)
        };
        assert_eq!(by_code(429, "insufficient_quota"), (Quota, false));
        assert_eq!(by_code(429, "usage_limit_reached"), (Quota, false));
        assert_eq!(by_code(429, "rate_limit_exceeded"), (RateLimit, true));
        assert_eq!(by_code(402, ""), (Quota, false));
        assert_eq!(by_code(400, "billing_error"), (Quota, false));
        assert_eq!(by_code(400, "content_filter"), (ContentFilter, false));
        assert_eq!(by_code(400, "content_policy_violation"), (ContentFilter, false));
        assert_eq!(by_code(400, "invalid_request_error"), (InvalidRequest, false));
        assert_eq!(kind(400, "Your credit balance is too low to access the Anthropic API"), (Quota, false));
    }

    #[test]
    fn the_new_kinds_serialise() {
        for k in [ProviderErrorKind::Quota, ProviderErrorKind::ContentFilter] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<ProviderErrorKind>(&json).unwrap(), k);
        }
    }
}
