//! AWS Signature Version 4, by hand, for the one thing Bedrock needs: a JSON POST signed with
//! a long-lived access key (and an optional session token). It is about sixty lines on top of
//! `hmac` and `sha2`, which are already in the lock file; the AWS SDK would bring dozens of
//! crates, an async runtime of its own and a second HTTP stack through `cargo deny`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aibus_provider::{ProviderError, ProviderErrorKind};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The pieces of an account's secret. The secret is stored as one string,
/// `ACCESS_KEY_ID:SECRET_ACCESS_KEY` with `:SESSION_TOKEN` after it for temporary credentials.
pub struct AwsKeys {
    pub access_key: String,
    secret_key: Zeroizing<String>,
    pub session_token: Option<Zeroizing<String>>,
}

impl AwsKeys {
    /// An error here never repeats the secret.
    pub fn parse(secret: &str) -> Result<Self, ProviderError> {
        let mut parts = secret.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(id), Some(key), token) if !id.is_empty() && !key.is_empty() => Ok(Self {
                access_key: id.to_owned(),
                secret_key: Zeroizing::new(key.to_owned()),
                session_token: token.filter(|t| !t.is_empty()).map(|t| Zeroizing::new(t.to_owned())),
            }),
            _ => Err(ProviderError::new(
                ProviderErrorKind::Auth,
                "the sigv4 secret must look like ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]",
            )),
        }
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Percent-encodes everything but the unreserved characters, as SigV4 wants.
pub fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `YYYYMMDD'T'HHMMSS'Z'` for a moment in time.
pub fn amz_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (the proleptic Gregorian algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, service.as_bytes());
    hmac(&k, b"aws4_request")
}

/// The `Authorization` header value. `path` and `query` are already in canonical form, and
/// `headers` are every header to sign, names lower-case and values trimmed (it must include
/// `host` and `x-amz-date`). `stamp` is the `amz_date` of the request.
pub fn authorization(
    keys: &AwsKeys,
    (region, service): (&str, &str),
    (method, path, query): (&str, &str, &str),
    headers: &[(&str, &str)],
    payload: &[u8],
    stamp: &str,
) -> String {
    let mut sorted = headers.to_vec();
    sorted.sort();
    let canonical_headers: String = sorted.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = sorted.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");
    let canonical = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed}\n{}",
        sha256_hex(payload)
    );
    let date = &stamp[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let signature = hex(&hmac(&signing_key(&keys.secret_key, date, region, service), to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        keys.access_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret and date of AWS's own worked example for deriving a signing key.
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    #[test]
    fn the_signing_key_matches_the_documented_example() {
        assert_eq!(
            hex(&signing_key(SECRET, "20150830", "us-east-1", "iam")),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    /// `get-vanilla` from the AWS SigV4 test suite.
    #[test]
    fn get_vanilla_matches_the_test_suite() {
        let keys = AwsKeys::parse(&format!("AKIDEXAMPLE:{SECRET}")).unwrap();
        let stamp = "20150830T123600Z";
        let auth = authorization(
            &keys,
            ("us-east-1", "service"),
            ("GET", "/", ""),
            &[("host", "example.amazonaws.com"), ("x-amz-date", stamp)],
            b"",
            stamp,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn dates_format_as_amz_dates() {
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        assert_eq!(amz_date(UNIX_EPOCH + Duration::from_secs(1_440_938_160)), "20150830T123600Z");
        // 2024-02-29, a leap day.
        assert_eq!(amz_date(UNIX_EPOCH + Duration::from_secs(1_709_210_096)), "20240229T123456Z");
    }

    #[test]
    fn a_bad_secret_is_an_auth_error_that_does_not_repeat_it() {
        let e = AwsKeys::parse("only-one-part-SECRETVALUE").err().unwrap();
        assert_eq!(e.kind, ProviderErrorKind::Auth);
        assert!(!e.message.contains("SECRETVALUE"));
        let keys = AwsKeys::parse("a:b:tok").unwrap();
        assert_eq!(keys.session_token.as_ref().map(|t| t.as_str()), Some("tok"));
    }

    #[test]
    fn colons_are_encoded() {
        assert_eq!(uri_encode("anthropic.claude-v1:0"), "anthropic.claude-v1%3A0");
        assert_eq!(uri_encode("a b~"), "a%20b~");
    }
}
