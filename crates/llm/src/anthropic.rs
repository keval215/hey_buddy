//! Anthropic Messages API provider (ADR 0002 sections 2, 3, 5, 8). Always cloud; the
//! transport is only called after the opt-in gate and a key lookup both succeed.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::Serialize;

use crate::credential::{CredentialStore, ANTHROPIC_ACCOUNT};
use crate::optin::CloudOptIn;
use crate::provider::{check_cloud_gate, LlmProvider};
use crate::request::{LlmRequest, Locality, PreparedRequest};
use crate::stream::{AnthropicSseParser, LlmDelta};
use crate::transport::{run_request, HeaderValue, HttpTransport, Timeouts};
use crate::LlmError;

pub const ANTHROPIC_PROVIDER_ID: &str = "anthropic";
/// Fixed; not configurable.
pub const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 1024;

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// Anthropic model id. Deliberately has no default: model ids change and an unverified
    /// default must not be baked in. The app supplies it from settings.
    pub model: String,
}

impl AnthropicConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

#[derive(Serialize)]
struct MessagesBody<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: [UserMessage<'a>; 1],
    stream: bool,
}

#[derive(Serialize)]
struct UserMessage<'a> {
    role: &'static str,
    content: Vec<Block<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Block<'a> {
    Image { source: ImageSource<'a> },
    Text { text: &'a str },
}

#[derive(Serialize)]
struct ImageSource<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: &'static str,
    data: &'a str,
}

pub struct AnthropicProvider {
    config: AnthropicConfig,
    transport: Arc<dyn HttpTransport>,
    credentials: Arc<dyn CredentialStore>,
    timeouts: Timeouts,
}

impl AnthropicProvider {
    pub fn with_transport(
        config: AnthropicConfig,
        transport: Arc<dyn HttpTransport>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, LlmError> {
        if config.model.trim().is_empty() {
            return Err(LlmError::Config("anthropic_model_missing"));
        }
        Ok(Self {
            config,
            transport,
            credentials,
            timeouts: Timeouts::cloud(),
        })
    }

    /// Real provider (reqwest + rustls, fixed https endpoint, no proxy).
    #[cfg(any(feature = "ollama", feature = "anthropic"))]
    pub fn new(
        config: AnthropicConfig,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, LlmError> {
        let t = crate::transport::ReqwestTransport::cloud(Timeouts::cloud())?;
        Self::with_transport(config, Arc::new(t), credentials)
    }

    pub fn with_timeouts(mut self, t: Timeouts) -> Self {
        self.timeouts = t;
        self
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        ANTHROPIC_PROVIDER_ID
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }

    fn prepare(&self, req: &LlmRequest) -> Result<PreparedRequest, LlmError> {
        let user = req.user_content();
        let b64 = req
            .image
            .as_ref()
            .map(|i| base64::engine::general_purpose::STANDARD.encode(&i.png[..]));
        let mut content = Vec::with_capacity(2);
        if let Some(data) = b64.as_deref() {
            content.push(Block::Image {
                source: ImageSource {
                    kind: "base64",
                    media_type: "image/png",
                    data,
                },
            });
        }
        content.push(Block::Text { text: &user });
        let body = MessagesBody {
            model: &self.config.model,
            max_tokens: MAX_TOKENS,
            system: &req.system,
            messages: [UserMessage {
                role: "user",
                content,
            }],
            stream: true,
        };
        let bytes = serde_json::to_vec(&body).map_err(|_| LlmError::Encode("body_serialize"))?;
        Ok(PreparedRequest::assemble(
            ANTHROPIC_PROVIDER_ID,
            ANTHROPIC_ENDPOINT,
            &self.config.model,
            Locality::Cloud,
            Bytes::from(bytes),
            req,
        ))
    }

    async fn stream(
        &self,
        req: PreparedRequest,
        opt_in: Option<CloudOptIn>,
    ) -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError> {
        // Order: provider/endpoint match, opt-in present, digest match, key present.
        check_cloud_gate(ANTHROPIC_PROVIDER_ID, ANTHROPIC_ENDPOINT, &req, opt_in)?;
        // Key is read per request and lives only in the sensitive header.
        let key = self
            .credentials
            .get(ANTHROPIC_ACCOUNT)?
            .ok_or(LlmError::NoCredentials)?;
        tracing::debug!(
            provider = ANTHROPIC_PROVIDER_ID,
            model = req.model(),
            body_bytes = req.body().len(),
            "llm stream start"
        );
        run_request(
            self.transport.as_ref(),
            ANTHROPIC_ENDPOINT,
            vec![
                ("x-api-key", HeaderValue::Sensitive(key)),
                ("anthropic-version", HeaderValue::Public(ANTHROPIC_VERSION)),
                ("content-type", HeaderValue::Public("application/json")),
            ],
            req.body().clone(),
            Box::new(AnthropicSseParser::new()),
            self.timeouts,
        )
        .await
    }
}
