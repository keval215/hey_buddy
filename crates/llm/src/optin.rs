//! Per-request cloud opt-in (ADR 0002 section 2). A [`CloudOptIn`] is bound to one prepared
//! request (digest, provider, endpoint), cannot be cloned, copied, forged or serialised, and
//! is consumed by `LlmProvider::stream`.
//!
//! CI greps that `CloudOptIn::for_request` appears only in `crates/core/src/consent.rs` and
//! in this crate.

use std::fmt;
use std::sync::Arc;

use crate::request::PreparedRequest;

/// How the user consented. Step 1 has exactly one way: pressing "Send" in the preview.
/// There is deliberately no always-allow / per-app bypass variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentSource {
    PreviewSend { request_id: u64 },
}

/// Proof that the user approved exactly one prepared request.
///
/// ```compile_fail,E0451
/// // Struct-literal forging is impossible: the fields are private (`todo!()` keeps type
/// // errors out of the way, so privacy is the only possible failure).
/// fn forge() -> llm::CloudOptIn {
///     llm::CloudOptIn {
///         digest: todo!(),
///         provider_id: todo!(),
///         endpoint: todo!(),
///         source: todo!(),
///     }
/// }
/// ```
///
/// ```compile_fail,E0382
/// // It is consumed by use: a second use of the same opt-in does not compile.
/// fn reuse(o: llm::CloudOptIn) {
///     let _first: Option<llm::CloudOptIn> = Some(o);
///     let _second: Option<llm::CloudOptIn> = Some(o);
/// }
/// ```
///
/// ```compile_fail,E0599
/// // Not Clone.
/// fn dup(o: llm::CloudOptIn) {
///     let _copy = o.clone();
/// }
/// ```
pub struct CloudOptIn {
    digest: [u8; 32],
    provider_id: Arc<str>,
    endpoint: Arc<str>,
    source: ConsentSource,
}

impl CloudOptIn {
    /// Mint an opt-in for exactly `req`. Only `crates/core/src/consent.rs` may call this
    /// outside this crate (CI-enforced).
    pub fn for_request(req: &PreparedRequest, source: ConsentSource) -> CloudOptIn {
        CloudOptIn {
            digest: req.digest(),
            provider_id: Arc::from(req.provider_id()),
            endpoint: Arc::from(req.endpoint()),
            source,
        }
    }

    pub fn source(&self) -> ConsentSource {
        self.source
    }

    /// Does this opt-in authorise exactly `req`?
    pub(crate) fn matches(&self, req: &PreparedRequest) -> bool {
        self.digest == req.digest()
            && *self.provider_id == *req.provider_id()
            && *self.endpoint == *req.endpoint()
    }
}

impl fmt::Debug for CloudOptIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ConsentSource::PreviewSend { request_id } = self.source;
        write!(f, "CloudOptIn(preview request {request_id})")
    }
}
