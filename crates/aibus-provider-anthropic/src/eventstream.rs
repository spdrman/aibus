//! The `application/vnd.amazon.eventstream` framing Bedrock streams with. Each frame is
//! `total length, headers length, prelude CRC, headers, payload, message CRC` (big-endian u32s,
//! CRC-32). A `chunk` event carries `{"bytes": "<base64 of one Messages API event>"}`; an
//! exception frame carries `{"message": ...}` and names its kind in `:exception-type`.

use aibus_provider::sse::SseEvent;
use aibus_provider::{ProviderError, ProviderErrorKind};
use base64::Engine;

#[derive(Default)]
pub struct Frames {
    buf: Vec<u8>,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn bad(what: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unknown, format!("malformed event stream: {what}"))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The status the same failure would have had as an HTTP answer.
fn status_of(exception: &str) -> u16 {
    match exception {
        "validationException" => 400,
        "accessDeniedException" => 403,
        "resourceNotFoundException" => 404,
        "throttlingException" | "serviceQuotaExceededException" => 429,
        "modelTimeoutException" => 504,
        "serviceUnavailableException" => 503,
        _ => 500,
    }
}

/// Header values are typed; only strings (type 7) matter here, the rest are skipped.
fn string_headers(mut h: &[u8]) -> Result<Vec<(String, String)>, ProviderError> {
    let mut out = Vec::new();
    while !h.is_empty() {
        let n = usize::from(h[0]);
        if h.len() < n + 2 {
            return Err(bad("short header"));
        }
        let name = String::from_utf8_lossy(&h[1..=n]).into_owned();
        let kind = h[n + 1];
        h = &h[n + 2..];
        let skip = match kind {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                if h.len() < 2 {
                    return Err(bad("short header"));
                }
                2 + usize::from(u16::from_be_bytes([h[0], h[1]]))
            }
            _ => return Err(bad("unknown header type")),
        };
        if h.len() < skip {
            return Err(bad("short header"));
        }
        if kind == 7 {
            out.push((name, String::from_utf8_lossy(&h[2..skip]).into_owned()));
        }
        h = &h[skip..];
    }
    Ok(out)
}

impl Frames {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next event, `None` when a whole frame has not arrived yet. An exception frame is
    /// the provider's error, mapped like the HTTP status it stands for.
    pub fn next(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        loop {
            if self.buf.len() < 12 {
                return Ok(None);
            }
            let (total, hlen) = (be32(&self.buf) as usize, be32(&self.buf[4..]) as usize);
            if total < 16 + hlen || total > 25 * 1024 * 1024 {
                return Err(bad("bad frame length"));
            }
            if self.buf.len() < total {
                return Ok(None);
            }
            let frame: Vec<u8> = self.buf.drain(..total).collect();
            if crc32(&frame[..8]) != be32(&frame[8..]) || crc32(&frame[..total - 4]) != be32(&frame[total - 4..]) {
                return Err(bad("checksum mismatch"));
            }
            let headers = string_headers(&frame[12..12 + hlen])?;
            let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
            let payload = &frame[12 + hlen..total - 4];
            if header(":message-type") == Some("exception") {
                let text = String::from_utf8_lossy(payload);
                let message = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| text.into_owned());
                let status = status_of(header(":exception-type").unwrap_or(""));
                return Err(ProviderError::from_http(status, &message, None, None, header(":exception-type").unwrap_or("")));
            }
            if header(":event-type") != Some("chunk") {
                continue;
            }
            let v: serde_json::Value = serde_json::from_slice(payload).map_err(|e| bad(&e.to_string()))?;
            let inner = base64::engine::general_purpose::STANDARD
                .decode(v["bytes"].as_str().ok_or_else(|| bad("chunk without bytes"))?)
                .map_err(|e| bad(&e.to_string()))?;
            let data = String::from_utf8(inner).map_err(|_| bad("event is not UTF-8"))?;
            return Ok(Some(SseEvent { event: String::new(), data }));
        }
    }

    /// True when bytes of a frame are left over, which means the stream was cut off.
    pub fn is_partial(&self) -> bool {
        !self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut h = Vec::new();
        for (k, v) in headers {
            h.push(k.len() as u8);
            h.extend_from_slice(k.as_bytes());
            h.push(7);
            h.extend_from_slice(&(v.len() as u16).to_be_bytes());
            h.extend_from_slice(v.as_bytes());
        }
        let total = (16 + h.len() + payload.len()) as u32;
        let mut f = total.to_be_bytes().to_vec();
        f.extend_from_slice(&(h.len() as u32).to_be_bytes());
        f.extend_from_slice(&crc32(&f).to_be_bytes());
        f.extend_from_slice(&h);
        f.extend_from_slice(payload);
        f.extend_from_slice(&crc32(&f).to_be_bytes());
        f
    }

    #[test]
    fn crc32_matches_the_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_chunk_frame_split_across_reads_gives_its_event() {
        let payload = br#"{"bytes":"eyJ0eXBlIjoicGluZyJ9"}"#;
        let f = frame(&[(":event-type", "chunk"), (":message-type", "event")], payload);
        let mut frames = Frames::default();
        frames.push(&f[..10]);
        assert!(frames.next().unwrap().is_none() && frames.is_partial());
        frames.push(&f[10..]);
        assert_eq!(frames.next().unwrap().unwrap().data, r#"{"type":"ping"}"#);
        assert!(frames.next().unwrap().is_none() && !frames.is_partial());
    }

    #[test]
    fn an_exception_frame_maps_like_its_http_status() {
        let f = frame(
            &[(":exception-type", "throttlingException"), (":message-type", "exception")],
            br#"{"message":"Too many requests"}"#,
        );
        let mut frames = Frames::default();
        frames.push(&f);
        let e = frames.next().unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::RateLimit);
        assert_eq!(e.message, "Too many requests");
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let mut f = frame(&[(":event-type", "chunk")], b"{}");
        let n = f.len();
        f[n - 6] ^= 0xFF;
        let mut frames = Frames::default();
        frames.push(&f);
        assert!(frames.next().unwrap_err().message.contains("checksum"));
    }
}
