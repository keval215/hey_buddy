use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::optin::CloudOptIn;
use crate::request::{LlmRequest, Locality, PreparedRequest};
use crate::stream::LlmDelta;
use crate::LlmError;

/// A language-model backend.
///
/// `prepare` is synchronous and pure: it fixes the exact bytes. `stream` sends exactly those
/// bytes. Cloud providers refuse to call the transport unless `opt_in` authorises this very
/// request; local providers ignore it.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn locality(&self) -> Locality;
    fn prepare(&self, req: &LlmRequest) -> Result<PreparedRequest, LlmError>;
    async fn stream(
        &self,
        req: PreparedRequest,
        opt_in: Option<CloudOptIn>,
    ) -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError>;
}

/// Cloud gate, in this order (every failure is an `Err` before any transport call):
/// 1. the request was prepared by this provider for this endpoint;
/// 2. an opt-in is present;
/// 3. the opt-in matches this request's digest, provider and endpoint.
///
/// The key check (4.) is done by the caller after this returns.
pub(crate) fn check_cloud_gate(
    provider_id: &str,
    endpoint: &str,
    req: &PreparedRequest,
    opt_in: Option<CloudOptIn>,
) -> Result<(), LlmError> {
    if req.provider_id() != provider_id
        || req.endpoint() != endpoint
        || req.locality() != Locality::Cloud
    {
        return Err(LlmError::Config("request_provider_mismatch"));
    }
    let opt_in = opt_in.ok_or(LlmError::NotOptedIn)?;
    if !opt_in.matches(req) {
        return Err(LlmError::NotOptedIn);
    }
    Ok(())
}

/// Local gate: the request must belong to this provider; the opt-in is ignored.
pub(crate) fn check_local(
    provider_id: &str,
    endpoint: &str,
    req: &PreparedRequest,
) -> Result<(), LlmError> {
    if req.provider_id() != provider_id
        || req.endpoint() != endpoint
        || req.locality() != Locality::Local
    {
        return Err(LlmError::Config("request_provider_mismatch"));
    }
    Ok(())
}
