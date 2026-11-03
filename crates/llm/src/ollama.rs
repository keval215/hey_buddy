//! Ollama `/api/chat` provider (ADR 0002 sections 3 and 5). Loopback only, plain http.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::Serialize;
use url::{Host, Url};

use crate::optin::CloudOptIn;
use crate::provider::{check_local, LlmProvider};
use crate::request::{LlmRequest, Locality, PreparedRequest};
use crate::stream::{LlmDelta, OllamaNdjsonParser};
use crate::transport::{run_request, HeaderValue, HttpTransport, Timeouts};
use crate::LlmError;

pub const OLLAMA_PROVIDER_ID: &str = "ollama";
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";
pub const DEFAULT_TEXT_MODEL: &str = "qwen3:4b";
pub const DEFAULT_VISION_MODEL: &str = "qwen3-vl:2b";
const KEEP_ALIVE_TEXT: &str = "10m";
const KEEP_ALIVE_VISION: &str = "1m";

#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Must be loopback http (see [`validate_loopback_base_url`]).
    pub base_url: String,
    pub text_model: String,
    /// `None` (low-memory profile): requests with an image fail with `Config("no_vision_model")`.
    pub vision_model: Option<String>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_BASE_URL.into(),
            text_model: DEFAULT_TEXT_MODEL.into(),
            vision_model: Some(DEFAULT_VISION_MODEL.into()),
        }
    }
}

/// Accepts only `http://127.0.0.1[:port]`, `http://[::1][:port]` and `http://localhost[:port]`
/// (rewritten to 127.0.0.1 so no DNS lookup ever happens), with an empty path, no credentials,
/// query or fragment. Returns the `/api/chat` endpoint.
pub fn validate_loopback_base_url(base: &str) -> Result<String, LlmError> {
    const NOT_LOOPBACK: LlmError = LlmError::Config("ollama_not_loopback");
    let url = Url::parse(base).map_err(|_| LlmError::Config("ollama_bad_url"))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(NOT_LOOPBACK);
    }
    let host = match url.host() {
        Some(Host::Ipv4(ip)) if ip == Ipv4Addr::LOCALHOST => "127.0.0.1".to_string(),
        Some(Host::Ipv6(ip)) if ip == Ipv6Addr::LOCALHOST => "[::1]".to_string(),
        Some(Host::Domain("localhost")) => "127.0.0.1".to_string(),
        _ => return Err(NOT_LOOPBACK),
    };
    let port = url.port_or_known_default().ok_or(NOT_LOOPBACK)?;
    Ok(format!("http://{host}:{port}/api/chat"))
}

#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    stream: bool,
    think: bool,
    keep_alive: &'a str,
    options: Options,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<[&'a str; 1]>,
}

#[derive(Serialize)]
struct Options {
    num_ctx: u32,
    num_predict: u32,
    temperature: f32,
}

pub struct OllamaProvider {
    endpoint: Arc<str>,
    config: OllamaConfig,
    transport: Arc<dyn HttpTransport>,
    timeouts: Timeouts,
}

impl OllamaProvider {
    /// Provider over an injected transport (tests, or a custom transport).
    pub fn with_transport(
        config: OllamaConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, LlmError> {
        let endpoint = validate_loopback_base_url(&config.base_url)?;
        if config.text_model.is_empty() || config.vision_model.as_deref() == Some("") {
            return Err(LlmError::Config("ollama_model_missing"));
        }
        Ok(Self {
            endpoint: Arc::from(endpoint),
            config,
            transport,
            timeouts: Timeouts::local(),
        })
    }

    /// Real provider over loopback http (reqwest).
    #[cfg(any(feature = "ollama", feature = "anthropic"))]
    pub fn new(config: OllamaConfig) -> Result<Self, LlmError> {
        let t = crate::transport::ReqwestTransport::local(Timeouts::local())?;
        Self::with_transport(config, Arc::new(t))
    }

    pub fn with_timeouts(mut self, t: Timeouts) -> Self {
        self.timeouts = t;
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn id(&self) -> &str {
        OLLAMA_PROVIDER_ID
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }

    fn prepare(&self, req: &LlmRequest) -> Result<PreparedRequest, LlmError> {
        let (model, keep_alive) = if req.has_image() {
            let m = self
                .config
                .vision_model
                .as_deref()
                .ok_or(LlmError::Config("no_vision_model"))?;
            (m, KEEP_ALIVE_VISION)
        } else {
            (self.config.text_model.as_str(), KEEP_ALIVE_TEXT)
        };
        let user = req.user_content();
        let b64 = req
            .image
            .as_ref()
            .map(|i| base64::engine::general_purpose::STANDARD.encode(&i.png[..]));
        let body = ChatBody {
            model,
            messages: [
                Message {
                    role: "system",
                    content: &req.system,
                    images: None,
                },
                Message {
                    role: "user",
                    content: &user,
                    images: b64.as_deref().map(|b| [b]),
                },
            ],
            stream: true,
            think: false,
            keep_alive,
            options: Options {
                num_ctx: 4096,
                num_predict: 512,
                temperature: 0.6,
            },
        };
        let bytes = serde_json::to_vec(&body).map_err(|_| LlmError::Encode("body_serialize"))?;
        Ok(PreparedRequest::assemble(
            OLLAMA_PROVIDER_ID,
            &self.endpoint,
            model,
            Locality::Local,
            Bytes::from(bytes),
            req,
        ))
    }

    async fn stream(
        &self,
        req: PreparedRequest,
        _opt_in: Option<CloudOptIn>,
    ) -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError> {
        check_local(OLLAMA_PROVIDER_ID, &self.endpoint, &req)?;
        tracing::debug!(
            provider = OLLAMA_PROVIDER_ID,
            model = req.model(),
            body_bytes = req.body().len(),
            "llm stream start"
        );
        run_request(
            self.transport.as_ref(),
            &self.endpoint,
            vec![("content-type", HeaderValue::Public("application/json"))],
            req.body().clone(),
            Box::new(OllamaNdjsonParser::new()),
            self.timeouts,
        )
        .await
    }
}
