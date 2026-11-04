//! HTTP transport abstraction, timeouts, status mapping and the delta pipeline
//! (ADR 0002 section 6). Providers talk to an injected [`HttpTransport`]; the real one
//! ([`ReqwestTransport`]) lives behind the `ollama` / `anthropic` features.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use secrecy::SecretString;
use tokio::time::{timeout_at, Instant};

use crate::stream::{DeltaParser, LlmDelta};
use crate::LlmError;

/// A header value. `Sensitive` values (API keys) are secrets: redacted in `Debug` and marked
/// sensitive on the wire.
pub enum HeaderValue {
    Public(&'static str),
    Sensitive(SecretString),
}

impl fmt::Debug for HeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderValue::Public(v) => write!(f, "Public({v:?})"),
            HeaderValue::Sensitive(_) => f.write_str("Sensitive([REDACTED])"),
        }
    }
}

/// Status plus the (lazy) response body.
pub struct HttpResponse {
    pub status: u16,
    pub body: BoxStream<'static, Result<Bytes, LlmError>>,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HttpResponse(status {})", self.status)
    }
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// POST `body` to `url`. Must not follow redirects, must not log or expose header values
    /// or the body in errors. Dropping the returned body stream cancels the request.
    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, HeaderValue)>,
        body: Bytes,
    ) -> Result<HttpResponse, LlmError>;
}

/// Timeouts (ADR: local 2/90/30/180 s, cloud 10/30/30/180 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// TCP/TLS connect (enforced by the real transport).
    pub connect: Duration,
    /// Time from request start until the first body chunk (also bounds `post`).
    pub first_chunk: Duration,
    /// Max silence between chunks after the first.
    pub idle: Duration,
    /// Whole call, from request start.
    pub total: Duration,
}

impl Timeouts {
    pub const fn local() -> Self {
        Self {
            connect: Duration::from_secs(2),
            first_chunk: Duration::from_secs(90),
            idle: Duration::from_secs(30),
            total: Duration::from_secs(180),
        }
    }
    pub const fn cloud() -> Self {
        Self {
            connect: Duration::from_secs(10),
            first_chunk: Duration::from_secs(30),
            idle: Duration::from_secs(30),
            total: Duration::from_secs(180),
        }
    }
}

/// 401/403 -> Auth, 429 -> RateLimited, other non-2xx -> Http. The body is never read.
pub(crate) fn map_status(status: u16) -> Result<(), LlmError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(LlmError::Auth),
        429 => Err(LlmError::RateLimited),
        s => Err(LlmError::Http { status: s }),
    }
}

/// POST with timeouts and status mapping; returns the parsed delta stream.
/// Dropping the returned stream drops the transport body stream (cancels the call).
pub(crate) async fn run_request(
    transport: &dyn HttpTransport,
    url: &str,
    headers: Vec<(&'static str, HeaderValue)>,
    body: Bytes,
    parser: Box<dyn DeltaParser>,
    timeouts: Timeouts,
) -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError> {
    let started = Instant::now();
    let post_deadline = started + timeouts.first_chunk.min(timeouts.total);
    let resp = timeout_at(post_deadline, transport.post(url, headers, body))
        .await
        .map_err(|_| LlmError::Timeout)??;
    map_status(resp.status)?;
    Ok(delta_stream(resp.body, parser, timeouts, started))
}

struct State {
    body: BoxStream<'static, Result<Bytes, LlmError>>,
    parser: Box<dyn DeltaParser>,
    pending: VecDeque<Result<LlmDelta, LlmError>>,
    started: Instant,
    got_first: bool,
    done: bool,
    t: Timeouts,
}

fn delta_stream(
    body: BoxStream<'static, Result<Bytes, LlmError>>,
    parser: Box<dyn DeltaParser>,
    t: Timeouts,
    started: Instant,
) -> BoxStream<'static, Result<LlmDelta, LlmError>> {
    let st = State {
        body,
        parser,
        pending: VecDeque::new(),
        started,
        got_first: false,
        done: false,
        t,
    };
    futures::stream::unfold(st, |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                // The first error or the terminal `Done` ends the stream.
                if item.is_err() || matches!(item, Ok(LlmDelta::Done { .. })) {
                    st.done = true;
                    st.pending.clear();
                    // Drop the transport stream now: cancels the request.
                    st.body = futures::stream::empty().boxed();
                }
                return Some((item, st));
            }
            if st.done {
                return None;
            }
            let wait_until = if st.got_first {
                Instant::now() + st.t.idle
            } else {
                st.started + st.t.first_chunk
            };
            let deadline = wait_until.min(st.started + st.t.total);
            match timeout_at(deadline, st.body.next()).await {
                Err(_) => st.pending.push_back(Err(LlmError::Timeout)),
                Ok(None) => {
                    let rest = st.parser.finish_stream();
                    st.pending.extend(rest);
                    st.done = true;
                }
                Ok(Some(Err(e))) => st.pending.push_back(Err(e)),
                Ok(Some(Ok(chunk))) => {
                    st.got_first = true;
                    let items = st.parser.push_bytes(&chunk);
                    st.pending.extend(items);
                }
            }
        }
    })
    .boxed()
}

#[cfg(any(feature = "ollama", feature = "anthropic"))]
pub use real::ReqwestTransport;

#[cfg(any(feature = "ollama", feature = "anthropic"))]
mod real {
    use std::sync::Once;

    use super::*;

    /// reqwest + rustls (ring provider, platform verifier). No redirects, no cookies.
    /// Local transports bypass every proxy; cloud transports use no system proxy either
    /// (Step 1). Errors carry fixed tokens only, never reqwest's text.
    pub struct ReqwestTransport {
        client: reqwest::Client,
    }

    static INSTALL_PROVIDER: Once = Once::new();

    /// `reqwest` is built with `rustls-no-provider`; install the `ring` provider once. If the
    /// application already installed another provider, keep that one.
    fn ensure_crypto_provider() {
        INSTALL_PROVIDER.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    impl ReqwestTransport {
        /// For loopback Ollama (plain http, connect timeout from `t`).
        pub fn local(t: Timeouts) -> Result<Self, LlmError> {
            Self::build(t.connect)
        }
        /// For HTTPS cloud providers.
        pub fn cloud(t: Timeouts) -> Result<Self, LlmError> {
            Self::build(t.connect)
        }

        fn build(connect: Duration) -> Result<Self, LlmError> {
            ensure_crypto_provider();
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(connect)
                .build()
                .map_err(|_| LlmError::Config("http_client"))?;
            Ok(Self { client })
        }
    }

    fn map_err(e: &reqwest::Error) -> LlmError {
        if e.is_timeout() {
            LlmError::Timeout
        } else if e.is_connect() {
            LlmError::Network("connect_failed")
        } else {
            LlmError::Network("request_failed")
        }
    }

    #[async_trait]
    impl HttpTransport for ReqwestTransport {
        async fn post(
            &self,
            url: &str,
            headers: Vec<(&'static str, super::HeaderValue)>,
            body: Bytes,
        ) -> Result<HttpResponse, LlmError> {
            use secrecy::ExposeSecret;
            let mut map = reqwest::header::HeaderMap::new();
            for (name, value) in headers {
                let (raw, sensitive) = match &value {
                    super::HeaderValue::Public(v) => (*v, false),
                    super::HeaderValue::Sensitive(s) => (s.expose_secret(), true),
                };
                let mut hv = reqwest::header::HeaderValue::from_str(raw)
                    .map_err(|_| LlmError::Config("invalid_header_value"))?;
                hv.set_sensitive(sensitive);
                map.insert(reqwest::header::HeaderName::from_static(name), hv);
            }
            let resp = self
                .client
                .post(url)
                .headers(map)
                .body(body)
                .send()
                .await
                .map_err(|e| map_err(&e))?;
            let status = resp.status().as_u16();
            let body = resp
                .bytes_stream()
                .map(|r| r.map_err(|e| map_err(&e)))
                .boxed();
            Ok(HttpResponse { status, body })
        }
    }
}
