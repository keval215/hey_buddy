//! Two-stage request (ADR 0002 section 1): a neutral [`LlmRequest`] becomes a
//! [`PreparedRequest`] (exact HTTP body bytes + digest) through `LlmProvider::prepare`.
//! The preview, the digest, the cloud opt-in and the transport all use that one object, so
//! previewed bytes == sent bytes.
//!
//! Screen content enters this crate only as [`firewall::RedactedFrame`] /
//! [`firewall::RedactedScreenText`]. There is no public API taking `frame::Frame`, raw image
//! bytes or raw tree text.

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use firewall::{RedactedFrame, RedactedScreenText};
use sha2::{Digest, Sha256};

use crate::image::{encode_frame, EncodedImage, ImageGeometry};
use crate::LlmError;

/// Domain-separation prefix of the request digest.
pub const DIGEST_DOMAIN: &[u8] = b"hey_buddy/llm/v1";

/// What the model may know about the screen.
///
/// ```compile_fail,E0308
/// // A raw `Frame` is not accepted where a `RedactedFrame` is expected.
/// fn f(raw: frame::Frame) -> llm::ScreenContext {
///     llm::ScreenContext::TextAndImage { text: None, image: raw }
/// }
/// ```
///
/// ```compile_fail,E0308
/// // A raw `String` is not accepted where a `RedactedScreenText` is expected.
/// fn f(raw: String) -> llm::ScreenContext {
///     llm::ScreenContext::Text(raw)
/// }
/// ```
#[derive(Debug)]
pub enum ScreenContext {
    /// Screen not shared (core adds a prompt note).
    None,
    /// DEFAULT: firewall-redacted accessibility-tree text.
    Text(RedactedScreenText),
    /// Deterministic fallback: a firewall-`Clear` frame, optionally with the text too.
    TextAndImage {
        text: Option<RedactedScreenText>,
        image: RedactedFrame,
    },
}

/// A closed JSON schema the reply must follow (ADR 0003 section 12, additive to ADR 0002).
/// Ollama gets it as the request's `format`; Anthropic gets it as text in the system prompt.
/// It is part of the request body, hence of the digest and of what the preview shows.
#[derive(Clone, PartialEq)]
pub struct OutputSchema {
    name: &'static str,
    schema: Arc<serde_json::Value>,
}

impl OutputSchema {
    pub fn new(name: &'static str, schema: serde_json::Value) -> Self {
        Self {
            name,
            schema: Arc::new(schema),
        }
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    pub fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    /// Compact JSON text of the schema.
    pub(crate) fn text(&self) -> String {
        self.schema.to_string()
    }
}

impl fmt::Debug for OutputSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OutputSchema({})", self.name)
    }
}

/// What a request is for. The preview panel shows it ("Agent planning" for `AgentPlan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPurpose {
    /// A normal voice turn.
    Turn,
    /// The agent engine asking for a plan (a request with an output schema).
    AgentPlan,
}

/// Provider-neutral request.
///
/// ```compile_fail,E0308
/// // `Frame` where a `ScreenContext` is expected.
/// fn f(raw: frame::Frame) -> Result<llm::LlmRequest, llm::LlmError> {
///     llm::LlmRequest::new(String::new(), String::new(), raw)
/// }
/// ```
///
/// ```compile_fail,E0308
/// // `String` where a `ScreenContext` is expected.
/// fn f(raw: String) -> Result<llm::LlmRequest, llm::LlmError> {
///     llm::LlmRequest::new(String::new(), String::new(), raw)
/// }
/// ```
pub struct LlmRequest {
    pub(crate) system: Arc<str>,
    pub(crate) transcript: Arc<str>,
    pub(crate) screen_text: Option<Arc<str>>,
    pub(crate) image: Option<EncodedImage>,
    pub(crate) secure_hidden: u32,
    pub(crate) output_schema: Option<OutputSchema>,
}

impl LlmRequest {
    /// Fails with `Encode(..)` only if an image was given and cannot be encoded within the
    /// size limit (core then rebuilds the request as `Text`/`None`).
    pub fn new(
        system: String,
        transcript: String,
        screen: ScreenContext,
    ) -> Result<Self, LlmError> {
        let (text, frame) = match screen {
            ScreenContext::None => (None, None),
            ScreenContext::Text(t) => (Some(t), None),
            ScreenContext::TextAndImage { text, image } => (text, Some(image)),
        };
        let image = frame.as_ref().map(encode_frame).transpose()?;
        let secure_hidden = match (&text, &frame) {
            (Some(t), _) => t.secure_fields_hidden(),
            (None, Some(f)) => u32::try_from(f.redactions().len()).unwrap_or(u32::MAX),
            (None, None) => 0,
        };
        Ok(Self {
            system: Arc::from(system),
            transcript: Arc::from(transcript),
            screen_text: text.map(|t| Arc::clone(t.text())),
            image,
            secure_hidden,
            output_schema: None,
        })
    }

    /// Require the reply to follow `schema` (agent planning). Makes the request's purpose
    /// [`RequestPurpose::AgentPlan`]: the schema is in the body, the digest and the preview.
    pub fn with_output_schema(mut self, schema: OutputSchema) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn output_schema(&self) -> Option<&OutputSchema> {
        self.output_schema.as_ref()
    }

    /// `AgentPlan` exactly when an output schema is set.
    pub fn purpose(&self) -> RequestPurpose {
        if self.output_schema.is_some() {
            RequestPurpose::AgentPlan
        } else {
            RequestPurpose::Turn
        }
    }

    /// Geometry of the attached image, if any.
    pub fn image_geometry(&self) -> Option<ImageGeometry> {
        self.image.as_ref().map(|i| i.geometry)
    }

    pub(crate) fn has_image(&self) -> bool {
        self.image.is_some()
    }

    /// The user message content: `<screen>\n` + screen text + `\n</screen>\n\n` + transcript
    /// when screen text is present, else the transcript.
    pub(crate) fn user_content(&self) -> String {
        match &self.screen_text {
            Some(t) => format!("<screen>\n{t}\n</screen>\n\n{}", self.transcript),
            None => self.transcript.to_string(),
        }
    }
}

impl fmt::Debug for LlmRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LlmRequest({:?}, system {} B, transcript {} B, screen_text {} B, image {})",
            self.purpose(),
            self.system.len(),
            self.transcript.len(),
            self.screen_text.as_ref().map_or(0, |t| t.len()),
            if self.image.is_some() { "yes" } else { "no" }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Local,
    Cloud,
}

/// `sha256("hey_buddy/llm/v1" || lp(provider_id) || lp(endpoint) || lp(model) || lp(body))`
/// with `lp(x)` = u64 little-endian length followed by `x`. Headers (the API key) are excluded.
pub(crate) fn compute_digest(
    provider_id: &str,
    endpoint: &str,
    model: &str,
    body: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DIGEST_DOMAIN);
    for part in [
        provider_id.as_bytes(),
        endpoint.as_bytes(),
        model.as_bytes(),
        body,
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().into()
}

/// Exact bytes to be POSTed plus everything needed to show and authorise them.
/// Private fields; only providers of this crate construct it.
#[derive(Clone)]
pub struct PreparedRequest {
    provider_id: Arc<str>,
    endpoint: Arc<str>,
    model: Arc<str>,
    locality: Locality,
    body: Bytes,
    digest: [u8; 32],
    system: Arc<str>,
    transcript: Arc<str>,
    screen_text: Option<Arc<str>>,
    image_png: Option<Arc<[u8]>>,
    image_geometry: Option<ImageGeometry>,
    secure_hidden: u32,
    purpose: RequestPurpose,
}

impl PreparedRequest {
    pub(crate) fn assemble(
        provider_id: &str,
        endpoint: &str,
        model: &str,
        locality: Locality,
        body: Bytes,
        req: &LlmRequest,
    ) -> Self {
        let digest = compute_digest(provider_id, endpoint, model, &body);
        Self {
            provider_id: Arc::from(provider_id),
            endpoint: Arc::from(endpoint),
            model: Arc::from(model),
            locality,
            body,
            digest,
            system: Arc::clone(&req.system),
            transcript: Arc::clone(&req.transcript),
            screen_text: req.screen_text.clone(),
            image_png: req.image.as_ref().map(|i| Arc::clone(&i.png)),
            image_geometry: req.image.as_ref().map(|i| i.geometry),
            secure_hidden: req.secure_hidden,
            purpose: req.purpose(),
        }
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn locality(&self) -> Locality {
        self.locality
    }
    /// The exact bytes that will be sent.
    pub fn body(&self) -> &Bytes {
        &self.body
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn purpose(&self) -> RequestPurpose {
        self.purpose
    }
    /// Shares the same `Bytes` / `Arc`s as the body (no copies).
    pub fn preview(&self) -> RequestPreview {
        RequestPreview {
            provider_id: Arc::clone(&self.provider_id),
            model: Arc::clone(&self.model),
            endpoint: Arc::clone(&self.endpoint),
            locality: self.locality,
            digest: self.digest,
            body: self.body.clone(),
            system: Arc::clone(&self.system),
            transcript: Arc::clone(&self.transcript),
            screen_text: self.screen_text.clone(),
            image_png: self.image_png.clone(),
            image_geometry: self.image_geometry,
            secure_fields_hidden: self.secure_hidden,
            purpose: self.purpose,
        }
    }
}

impl fmt::Debug for PreparedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PreparedRequest({} {:?} model {} body {} B)",
            self.provider_id,
            self.locality,
            self.model,
            self.body.len()
        )
    }
}

/// What the preview panel shows. `Debug` prints ids and sizes only (never content);
/// no serde: the UI gets explicit, separately-reviewed projections.
#[derive(Clone)]
pub struct RequestPreview {
    pub provider_id: Arc<str>,
    pub model: Arc<str>,
    pub endpoint: Arc<str>,
    pub locality: Locality,
    pub digest: [u8; 32],
    pub body: Bytes,
    /// Added to the ADR struct so the panel (and tests) can show/verify the system prompt.
    pub system: Arc<str>,
    pub transcript: Arc<str>,
    pub screen_text: Option<Arc<str>>,
    pub image_png: Option<Arc<[u8]>>,
    pub image_geometry: Option<ImageGeometry>,
    pub secure_fields_hidden: u32,
    /// `AgentPlan` for the agent engine's planning calls (the panel says "Agent planning").
    pub purpose: RequestPurpose,
}

impl RequestPreview {
    pub fn digest_hex(&self) -> String {
        self.digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Debug for RequestPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RequestPreview({:?} {} {:?} model {} digest {} body {} B, transcript {} B, \
             screen_text {} B, image {} B, {} secure hidden)",
            self.purpose,
            self.provider_id,
            self.locality,
            self.model,
            &self.digest_hex()[..8],
            self.body.len(),
            self.transcript.len(),
            self.screen_text.as_ref().map_or(0, |t| t.len()),
            self.image_png.as_ref().map_or(0, |i| i.len()),
            self.secure_fields_hidden
        )
    }
}
