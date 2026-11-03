//! Test doubles (feature `fakes`): `FakeTransport`, `FakeCredentialStore`, `FakeLlm`.
//! No sockets, no files.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};

use crate::credential::CredentialStore;
use crate::optin::CloudOptIn;
use crate::provider::{check_cloud_gate, LlmProvider};
use crate::request::{LlmRequest, Locality, PreparedRequest};
use crate::stream::LlmDelta;
use crate::transport::{HeaderValue, HttpResponse, HttpTransport};
use crate::LlmError;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One recorded `post`.
#[derive(Clone)]
pub struct RecordedCall {
    pub url: String,
    /// `(name, value, sensitive)`. The value of a sensitive header IS recorded here (tests
    /// assert the key appears only in headers); `Debug` redacts it.
    pub headers: Vec<(&'static str, String, bool)>,
    pub body: Bytes,
}

impl std::fmt::Debug for RecordedCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RecordedCall({}, {} headers, {} B)",
            self.url,
            self.headers.len(),
            self.body.len()
        )
    }
}

#[derive(Clone)]
enum Behavior {
    Respond {
        status: u16,
        chunks: Vec<Bytes>,
    },
    /// Yield `chunks`, then never produce anything again.
    StallBody {
        chunks: Vec<Bytes>,
    },
    /// `post` never completes.
    StallPost,
    Fail(LlmError),
}

/// Scripted transport. Records every call; the response is the configured behaviour.
pub struct FakeTransport {
    calls: Mutex<Vec<RecordedCall>>,
    behavior: Behavior,
    drop_flag: Arc<AtomicBool>,
    count: AtomicUsize,
}

impl FakeTransport {
    fn with(behavior: Behavior) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            behavior,
            drop_flag: Arc::new(AtomicBool::new(false)),
            count: AtomicUsize::new(0),
        }
    }
    /// Answers with `status` and the given body chunks.
    pub fn responding(status: u16, chunks: Vec<Vec<u8>>) -> Self {
        Self::with(Behavior::Respond {
            status,
            chunks: chunks.into_iter().map(Bytes::from).collect(),
        })
    }
    /// Answers 200 then stalls forever after `chunks`.
    pub fn stalling_body(chunks: Vec<Vec<u8>>) -> Self {
        Self::with(Behavior::StallBody {
            chunks: chunks.into_iter().map(Bytes::from).collect(),
        })
    }
    /// `post` never returns.
    pub fn stalling_post() -> Self {
        Self::with(Behavior::StallPost)
    }
    pub fn failing(e: LlmError) -> Self {
        Self::with(Behavior::Fail(e))
    }
    pub fn calls(&self) -> Vec<RecordedCall> {
        lock(&self.calls).clone()
    }
    pub fn call_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
    /// Set to `true` when a response body stream returned by this transport is dropped.
    pub fn body_dropped_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.drop_flag)
    }
}

struct FlaggedBody {
    chunks: VecDeque<Bytes>,
    stall_at_end: bool,
    flag: Arc<AtomicBool>,
}

impl Stream for FlaggedBody {
    type Item = Result<Bytes, LlmError>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.chunks.pop_front() {
            Some(c) => Poll::Ready(Some(Ok(c))),
            None if self.stall_at_end => Poll::Pending,
            None => Poll::Ready(None),
        }
    }
}

impl Drop for FlaggedBody {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, HeaderValue)>,
        body: Bytes,
    ) -> Result<HttpResponse, LlmError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        lock(&self.calls).push(RecordedCall {
            url: url.to_string(),
            headers: headers
                .iter()
                .map(|(n, v)| match v {
                    HeaderValue::Public(s) => (*n, (*s).to_string(), false),
                    HeaderValue::Sensitive(s) => (*n, s.expose_secret().to_string(), true),
                })
                .collect(),
            body,
        });
        let (status, chunks, stall) = match &self.behavior {
            Behavior::Respond { status, chunks } => (*status, chunks.clone(), false),
            Behavior::StallBody { chunks } => (200, chunks.clone(), true),
            Behavior::StallPost => futures::future::pending().await,
            Behavior::Fail(e) => return Err(e.clone()),
        };
        Ok(HttpResponse {
            status,
            body: FlaggedBody {
                chunks: chunks.into(),
                stall_at_end: stall,
                flag: Arc::clone(&self.drop_flag),
            }
            .boxed(),
        })
    }
}

/// In-memory key store.
#[derive(Default)]
pub struct FakeCredentialStore {
    keys: Mutex<HashMap<String, String>>,
    gets: AtomicUsize,
}

impl FakeCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_key(account: &str, key: &str) -> Self {
        let s = Self::new();
        lock(&s.keys).insert(account.to_string(), key.to_string());
        s
    }
    pub fn get_count(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }
}

impl CredentialStore for FakeCredentialStore {
    fn get(&self, account: &str) -> Result<Option<SecretString>, LlmError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        Ok(lock(&self.keys)
            .get(account)
            .map(|k| SecretString::from(k.clone())))
    }
    fn set(&self, account: &str, key: SecretString) -> Result<(), LlmError> {
        lock(&self.keys).insert(account.to_string(), key.expose_secret().to_string());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), LlmError> {
        lock(&self.keys).remove(account);
        Ok(())
    }
}

/// Scripted provider for core tests. `prepare` builds a small JSON body from the request;
/// a cloud `FakeLlm` enforces the same opt-in gate as the real cloud adapter.
pub struct FakeLlm {
    id: String,
    locality: Locality,
    deltas: Vec<LlmDelta>,
    streams: AtomicUsize,
}

impl FakeLlm {
    pub fn local(deltas: Vec<LlmDelta>) -> Self {
        Self::new("fake-local", Locality::Local, deltas)
    }
    pub fn cloud(deltas: Vec<LlmDelta>) -> Self {
        Self::new("fake-cloud", Locality::Cloud, deltas)
    }
    fn new(id: &str, locality: Locality, deltas: Vec<LlmDelta>) -> Self {
        Self {
            id: id.into(),
            locality,
            deltas,
            streams: AtomicUsize::new(0),
        }
    }
    /// Number of `stream` calls that passed the gate.
    pub fn stream_count(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }
    fn endpoint(&self) -> String {
        format!("fake://{}", self.id)
    }
}

#[async_trait]
impl LlmProvider for FakeLlm {
    fn id(&self) -> &str {
        &self.id
    }
    fn locality(&self) -> Locality {
        self.locality
    }
    fn prepare(&self, req: &LlmRequest) -> Result<PreparedRequest, LlmError> {
        let body = serde_json::json!({
            "system": &*req.system,
            "user": req.user_content(),
            "image_bytes": req.image.as_ref().map(|i| i.png.len()),
        });
        let bytes = serde_json::to_vec(&body).map_err(|_| LlmError::Encode("body_serialize"))?;
        Ok(PreparedRequest::assemble(
            &self.id,
            &self.endpoint(),
            "fake-model",
            self.locality,
            Bytes::from(bytes),
            req,
        ))
    }
    async fn stream(
        &self,
        req: PreparedRequest,
        opt_in: Option<CloudOptIn>,
    ) -> Result<BoxStream<'static, Result<LlmDelta, LlmError>>, LlmError> {
        if self.locality == Locality::Cloud {
            check_cloud_gate(&self.id, &self.endpoint(), &req, opt_in)?;
        }
        self.streams.fetch_add(1, Ordering::SeqCst);
        Ok(futures::stream::iter(self.deltas.clone().into_iter().map(Ok)).boxed())
    }
}
