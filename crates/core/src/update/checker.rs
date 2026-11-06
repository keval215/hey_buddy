//! The one network call of an update check (ADR 0005 section 5.1).

use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use reqwest::header::ACCEPT_ENCODING;
use reqwest::{Client, Url};
use update::{TrustConfig, UpdateError, MAX_ENVELOPE_BYTES};

use super::UpdateFail;
use crate::download::fetch::{ensure_crypto_provider, host_ok, target_ok};
use crate::download::DownloadError;

/// The only `User-Agent` the check sends.
pub const USER_AGENT: &str = "hey_buddy-update";
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Whole-request ceiling (the body is at most 64 KiB).
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Fetches the signed envelope. EXACTLY ONE GET per call; the result is untrusted bytes that
/// only `update::verify` can turn into anything.
#[async_trait]
pub trait UpdateChecker: Send + Sync {
    async fn fetch_envelope(&self, cfg: &TrustConfig) -> Result<Bytes, UpdateFail>;
}

/// reqwest + rustls(ring), same client policy as the downloader plus the host allowlist:
/// HTTPS only, no cookies, no query string, no version / OS in the request, fixed user agent,
/// at most 5 redirects each re-checked against the allowlist, body capped at 64 KiB.
pub struct HttpUpdateChecker {
    /// Only ever true through [`HttpUpdateChecker::for_tests`].
    allow_loopback_http: bool,
}

impl Default for HttpUpdateChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpUpdateChecker {
    pub fn new() -> Self {
        Self {
            allow_loopback_http: false,
        }
    }

    /// Test constructor (compiled only for tests): allows `http://127.0.0.1`.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            allow_loopback_http: true,
        }
    }
}

#[async_trait]
impl UpdateChecker for HttpUpdateChecker {
    async fn fetch_envelope(&self, cfg: &TrustConfig) -> Result<Bytes, UpdateFail> {
        ensure_crypto_provider();
        let allow_http = self.allow_loopback_http;
        let hosts: Vec<String> = cfg.hosts.iter().map(|h| (*h).to_string()).collect();
        let url = Url::parse(cfg.manifest_url).map_err(|_| DownloadError::InvalidUrl)?;
        // No query string, fragment or userinfo; https (or test loopback); listed host.
        if url.query().is_some()
            || url.fragment().is_some()
            || !target_ok(&url, allow_http)
            || !host_ok(&url, Some(&hosts), allow_http)
        {
            return Err(DownloadError::InvalidUrl.into());
        }
        let redirect_hosts = hosts.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS
                || !target_ok(attempt.url(), allow_http)
                || !host_ok(attempt.url(), Some(&redirect_hosts), allow_http)
            {
                attempt.error("redirect")
            } else {
                attempt.follow()
            }
        });
        let mut b = Client::builder()
            .redirect(policy)
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .https_only(!allow_http);
        if allow_http {
            b = b.no_proxy();
        }
        let client = b.build().map_err(|_| DownloadError::Network)?;
        let resp = client
            .get(url)
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|e| {
                if e.is_redirect() {
                    DownloadError::BadRedirect
                } else {
                    DownloadError::Network
                }
            })?;
        if resp.status() != reqwest::StatusCode::OK {
            return Err(DownloadError::HttpStatus.into());
        }
        // A declared length over the cap aborts before a single body byte is read.
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_ENVELOPE_BYTES as u64)
        {
            return Err(UpdateError::TooLarge.into());
        }
        let mut body = BytesMut::new();
        let mut stream = Box::pin(resp.bytes_stream());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| DownloadError::Network)?;
            if body.len() + chunk.len() > MAX_ENVELOPE_BYTES {
                return Err(UpdateError::TooLarge.into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }
}

/// Canned bytes; counts calls (tests, `fakes` feature only).
#[cfg(any(test, feature = "fakes"))]
pub struct FakeUpdateChecker {
    reply: std::sync::Mutex<Result<Vec<u8>, UpdateFail>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeUpdateChecker {
    pub fn new(envelope: Vec<u8>) -> Self {
        Self {
            reply: std::sync::Mutex::new(Ok(envelope)),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn failing(e: UpdateFail) -> Self {
        Self {
            reply: std::sync::Mutex::new(Err(e)),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn set(&self, envelope: Vec<u8>) {
        *self.reply.lock().unwrap_or_else(|p| p.into_inner()) = Ok(envelope);
    }

    pub fn set_error(&self, e: UpdateFail) {
        *self.reply.lock().unwrap_or_else(|p| p.into_inner()) = Err(e);
    }

    /// How many times `fetch_envelope` ran.
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "fakes"))]
#[async_trait]
impl UpdateChecker for FakeUpdateChecker {
    async fn fetch_envelope(&self, _cfg: &TrustConfig) -> Result<Bytes, UpdateFail> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.reply
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .map(Bytes::from)
    }
}
