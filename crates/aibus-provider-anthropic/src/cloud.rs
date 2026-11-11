//! Claude over Bedrock (`auth = sigv4`) and Vertex AI (`auth = gcloud`): the same Messages API
//! body, a different URL, a different way to authenticate and, for Bedrock, a different
//! framing of the stream (04 section 3, 09 section 8). Both are off unless a caller builds the
//! provider with `Anthropic::bedrock` or `Anthropic::vertex`, or through `CloudFactory` from the
//! `bedrock` and `vertex` presets; the plain `Factory` never does.
//!
//! The AWS secret and the Google token reach the wire only as sensitive headers, and neither
//! the signed header set nor the token is logged.

use std::sync::Arc;
use std::time::SystemTime;

use aibus_provider::{
    Api, ConfigError, Credential, ModelId, Provider, ProviderConfig, ProviderError, ProviderErrorKind,
    ProviderFactory, ProviderId, Secret,
};
use aibus_provider::AuthMethodKind;
use aibus_provider_http::reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use aibus_provider_http::reqwest::Url;
use aibus_provider_http::sensitive;
use async_trait::async_trait;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::sigv4::{amz_date, authorization, uri_encode, AwsKeys};
use crate::{Anthropic, EFFORT_BETA};

/// Where Vertex's access token comes from: `gcloud auth print-access-token` in a real run, a
/// fixed token in a test. It is asked for on every request, on behalf of the account the request
/// is for (`creds.account`), so a token that expired is replaced by whoever implements this and
/// a cache can key on the account. The secret in `creds` is not the token.
#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self, creds: &Credential) -> Result<Secret<String>, ProviderError>;
}

/// Builds the `anthropic-bedrock` and `anthropic-vertex` apis from a provider definition (the
/// `bedrock` and `vertex` presets, or a `[[provider]]` with one of those apis). `tokens` is the
/// binary's Vertex token source; Bedrock needs none, since it signs with the account's secret.
pub struct CloudFactory {
    pub tokens: Arc<dyn TokenSource>,
}

impl ProviderFactory for CloudFactory {
    fn id(&self) -> ProviderId {
        ProviderId("anthropic-cloud")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        let def = cfg.def.as_ref().ok_or_else(|| {
            ConfigError::NotConfigured("anthropic-cloud".into(), "built from a bedrock or vertex definition only".into())
        })?;
        let need = |field: &Option<String>, what: &str| {
            field.as_deref().ok_or_else(|| {
                ConfigError::Invalid(def.id.clone(), format!("{what} is required: set it on the account or on the [[provider]]"))
            })
        };
        let region = need(&def.region, "region")?;
        match def.api {
            Some(Api::AnthropicBedrock) => Ok(Arc::new(Anthropic::bedrock(cfg, region, SystemTime::now)?)),
            Some(Api::AnthropicVertex) => {
                Ok(Arc::new(Anthropic::vertex(cfg, need(&def.project, "project")?, region, self.tokens.clone())?))
            }
            other => Err(ConfigError::Invalid(def.id.clone(), format!("api {other:?} is not a cloud api"))),
        }
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey, AuthMethodKind::Bearer, AuthMethodKind::Env, AuthMethodKind::None]
    }

    fn apis(&self) -> &'static [&'static str] {
        &["anthropic-bedrock", "anthropic-vertex"]
    }
}

pub(crate) enum Transport {
    Direct,
    Bedrock { region: String, now: Box<dyn Fn() -> SystemTime + Send + Sync> },
    Vertex { project: String, region: String, tokens: Arc<dyn TokenSource> },
}

fn invalid(what: &str) -> ConfigError {
    ConfigError::Invalid("anthropic".into(), what.into())
}

/// A region lands in a host name, so it is held to what a region looks like.
fn region_ok(r: &str) -> Result<(), ConfigError> {
    if !r.is_empty() && r.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        Ok(())
    } else {
        Err(invalid("region must be lower-case letters, digits and dashes"))
    }
}

impl Anthropic {
    /// Bedrock Runtime in `region`. `base_url` overrides the default
    /// `https://bedrock-runtime.<region>.amazonaws.com`. Calls are signed with the account's
    /// secret (`ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]`); `now` is the clock the
    /// signature is stamped with.
    pub fn bedrock(
        cfg: &ProviderConfig,
        region: &str,
        now: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> Result<Self, ConfigError> {
        region_ok(region)?;
        Self::with_transport(
            cfg,
            format!("https://bedrock-runtime.{region}.amazonaws.com"),
            Transport::Bedrock { region: region.to_owned(), now: Box::new(now) },
            // The bare Anthropic id, or an inference profile (`us.anthropic.claude-...`).
            &["claude-", "anthropic.", "us.anthropic.", "eu.anthropic.", "apac.anthropic.", "global.anthropic."],
        )
    }

    /// Vertex AI for `project` in `region` (`global` for the global endpoint). `base_url`
    /// overrides the default `https://<region>-aiplatform.googleapis.com`.
    pub fn vertex(
        cfg: &ProviderConfig,
        project: &str,
        region: &str,
        tokens: Arc<dyn TokenSource>,
    ) -> Result<Self, ConfigError> {
        region_ok(region)?;
        if project.is_empty() || !project.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')) {
            return Err(invalid("project must be a Google Cloud project id"));
        }
        let host = if region == "global" {
            "aiplatform.googleapis.com".to_owned()
        } else {
            format!("{region}-aiplatform.googleapis.com")
        };
        Self::with_transport(
            cfg,
            format!("https://{host}"),
            Transport::Vertex { project: project.to_owned(), region: region.to_owned(), tokens },
            &["claude-"],
        )
    }

    /// The URL, headers and body for one streaming call. `body` is the Messages API body as
    /// the direct API takes it; the cloud transports move the model into the URL.
    pub(crate) async fn prepare(
        &self,
        creds: &Credential,
        model: &ModelId,
        mut body: Value,
        beta: bool,
    ) -> Result<(String, HeaderMap, String), ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match &self.transport {
            Transport::Direct => {
                headers.extend(self.headers(creds)?);
                headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
                if beta {
                    headers.insert("anthropic-beta", HeaderValue::from_static(EFFORT_BETA));
                }
                Ok((format!("{}/v1/messages", self.base), headers, body.to_string()))
            }
            Transport::Bedrock { region, now } => {
                let obj = body.as_object_mut().expect("the body is an object");
                obj.remove("model");
                obj.remove("stream");
                obj.insert("anthropic_version".into(), json!("bedrock-2023-05-31"));
                if beta {
                    obj.insert("anthropic_beta".into(), json!([EFFORT_BETA]));
                }
                let body = body.to_string();

                // The path is encoded once on the wire and once more in the canonical request.
                let model = uri_encode(&model.0);
                let tail = "invoke-with-response-stream";
                let url = format!("{}/model/{model}/{tail}", self.base);
                headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.amazon.eventstream"));
                // A Bedrock API key (`AWS_BEARER_TOKEN_BEDROCK`) has no colon; an access key pair
                // always does. The key is a bearer token and nothing is signed.
                if !creds.secret.expose().contains(':') {
                    let value = Zeroizing::new(format!("Bearer {}", creds.secret.expose()));
                    headers.insert(AUTHORIZATION, sensitive(&value)?);
                    return Ok((url, headers, body));
                }
                let keys = AwsKeys::parse(creds.secret.expose())?;
                let canonical = format!("/model/{}/{tail}", uri_encode(&model));
                let host = Url::parse(&url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| u.port().map_or(h.to_owned(), |p| format!("{h}:{p}"))))
                    .ok_or_else(|| ProviderError::new(ProviderErrorKind::Unknown, "base_url has no host"))?;
                let stamp = amz_date(now());
                let token = keys.session_token.as_ref().map(|t| Zeroizing::new(t.to_string()));
                let mut signed = vec![("content-type", "application/json"), ("host", host.as_str()), ("x-amz-date", stamp.as_str())];
                if let Some(t) = &token {
                    signed.push(("x-amz-security-token", t.as_str()));
                }
                let auth = Zeroizing::new(authorization(
                    &keys,
                    (region.as_str(), "bedrock"),
                    ("POST", canonical.as_str(), ""),
                    &signed,
                    body.as_bytes(),
                    &stamp,
                ));
                headers.insert("x-amz-date", sensitive(&stamp)?);
                if let Some(t) = &token {
                    headers.insert("x-amz-security-token", sensitive(t)?);
                }
                headers.insert(AUTHORIZATION, sensitive(&auth)?);
                Ok((url, headers, body))
            }
            Transport::Vertex { project, region, tokens } => {
                let obj = body.as_object_mut().expect("the body is an object");
                obj.remove("model");
                obj.insert("anthropic_version".into(), json!("vertex-2023-10-16"));
                if beta {
                    obj.insert("anthropic_beta".into(), json!([EFFORT_BETA]));
                }
                let token = tokens.token(creds).await?;
                let value = Zeroizing::new(format!("Bearer {}", token.expose()));
                headers.insert(AUTHORIZATION, sensitive(&value)?);
                headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
                let path = format!("v1/projects/{project}/locations/{region}/publishers/anthropic/models/{}:streamRawPredict", model.0);
                Ok((format!("{}/{path}", self.base), headers, body.to_string()))
            }
        }
    }
}
