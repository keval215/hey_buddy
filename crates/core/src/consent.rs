//! The consent gate (ADR 0002 sections 2 and A3/A7, decision D-003).
//!
//! Every cloud call needs a [`CloudOptIn`]. This file is the ONLY place outside `crates/llm`
//! that mints one (`CloudOptIn::for_request`; CI greps for it). The opt-in comes out of
//! [`PendingConsent::wait`] only after the user pressed "Send" in the preview panel for exactly
//! this [`PreparedRequest`]:
//!
//! - [`ConsentGate::request_consent`] registers a pending request under a fresh `request_id`,
//!   stores the previewed bytes in the in-memory [`PreviewStore`] and shows the panel through
//!   the [`PreviewSink`];
//! - [`ConsentGate::decide`] resolves it; decisions are single use, and an unknown, expired,
//!   already decided or superseded id is an error;
//! - timeout, [`ConsentGate::cancel_all`] (turn cancel, a newer turn, app quit) and any failure
//!   to show the panel mean Cancel;
//! - with `local_only` on the gate never mints anything (`LocalOnly`);
//! - there is no "always allow" and no per-app bypass.
//!
//! Local providers never come here: [`provider_allowed`] and `Buddy::prepare_and_consent`
//! skip the panel for them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use llm::{CloudOptIn, ConsentSource, Locality, PreparedRequest, RequestPreview};
use tokio::sync::oneshot;

use crate::api::{Decision, LastRequestDto, LocalityDto, PreviewDto, Settings};
use crate::error::CoreError;

/// How long a preview waits for a decision before it is cancelled.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Store key of the most recent turn's request (`preview://last`).
pub const LAST_KEY: &str = "last";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why no opt-in was produced. `Display` is a fixed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConsentError {
    #[error("local_only")]
    LocalOnly,
    /// Consent is only for cloud requests.
    #[error("not_cloud")]
    NotCloud,
    /// Unknown, expired, already decided or superseded request id.
    #[error("unknown_request")]
    UnknownRequest,
    #[error("cancelled")]
    Cancelled,
    #[error("timed_out")]
    TimedOut,
    /// The panel could not be shown (fail closed).
    #[error("ui_unavailable")]
    UiUnavailable,
    #[error("digest_mismatch")]
    DigestMismatch,
}

impl From<ConsentError> for CoreError {
    fn from(e: ConsentError) -> Self {
        match e {
            ConsentError::LocalOnly => CoreError::LocalOnly,
            _ => CoreError::Cancelled,
        }
    }
}

/// `Ok` when a provider of this locality may be used at all under `settings`.
/// Cloud with `local_only` is refused here, before anything is prepared or shown.
pub fn provider_allowed(settings: &Settings, locality: Locality) -> Result<(), CoreError> {
    match locality {
        Locality::Local => Ok(()),
        Locality::Cloud if settings.local_only => Err(CoreError::LocalOnly),
        Locality::Cloud => Ok(()),
    }
}

/// How the glue shows and hides the preview panel.
pub trait PreviewSink: Send + Sync {
    /// Show the panel with `dto`. `false` = it could not be shown (the request is cancelled).
    fn show(&self, dto: PreviewDto) -> bool;
    /// The pending preview ended; hide and clear the panel.
    fn close(&self, request_id: u64);
}

/// Default sink: cannot show anything, so every cloud request is cancelled (fail closed).
pub struct NoSink;

impl PreviewSink for NoSink {
    fn show(&self, _dto: PreviewDto) -> bool {
        false
    }
    fn close(&self, _request_id: u64) {}
}

// ---------------------------------------------------------------- preview store

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// `preview://<id>`: the PNG.
    Image,
    /// `preview://<id>/body`: the exact JSON body.
    Body,
}

/// One served response body.
#[derive(Debug, Clone)]
pub struct Asset {
    pub content_type: &'static str,
    /// Shares the allocation of the request's `Arc<[u8]>` / `Bytes` (no copy).
    pub bytes: Bytes,
}

struct Entry {
    image: Option<Arc<[u8]>>,
    body: Bytes,
}

/// In-memory copies (Arcs) of the previewed request, keyed by request id or [`LAST_KEY`].
/// Never written to disk.
#[derive(Default)]
pub struct PreviewStore {
    entries: Mutex<HashMap<String, Entry>>,
}

impl PreviewStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold the preview's image `Arc` and body `Bytes` (the same ones the request carries).
    pub fn put(&self, key: &str, p: &RequestPreview) {
        lock(&self.entries).insert(
            key.to_string(),
            Entry {
                image: p.image_png.clone(),
                body: p.body.clone(),
            },
        );
    }

    pub fn remove(&self, key: &str) {
        lock(&self.entries).remove(key);
    }

    pub fn contains(&self, key: &str) -> bool {
        lock(&self.entries).contains_key(key)
    }

    /// The very `Arc` of the image (for identity checks).
    pub fn image_arc(&self, key: &str) -> Option<Arc<[u8]>> {
        lock(&self.entries).get(key)?.image.clone()
    }

    pub fn asset(&self, key: &str, kind: AssetKind) -> Option<Asset> {
        let g = lock(&self.entries);
        let e = g.get(key)?;
        match kind {
            AssetKind::Image => Some(Asset {
                content_type: "image/png",
                bytes: Bytes::from_owner(Arc::clone(e.image.as_ref()?)),
            }),
            AssetKind::Body => Some(Asset {
                content_type: "application/json",
                bytes: e.body.clone(),
            }),
        }
    }

    /// Resolve a `preview` URI. `host` and `path` come from the request URI: macOS/Linux use
    /// `preview://<key>[/body]`, Windows serves custom schemes as `http://preview.localhost/<key>[/body]`.
    /// Unknown shapes, keys and missing entries give `None` (404).
    pub fn lookup(&self, host: Option<&str>, path: &str) -> Option<Asset> {
        let (key, kind) = parse_uri(host, path)?;
        self.asset(&key, kind)
    }
}

/// `(key, kind)` of a preview URI, or `None` when it has an unknown shape.
pub fn parse_uri(host: Option<&str>, path: &str) -> Option<(String, AssetKind)> {
    let path = path.trim_start_matches('/');
    let (key, rest) = match host {
        None | Some("") | Some("localhost") | Some("preview.localhost") => {
            path.split_once('/').unwrap_or((path, ""))
        }
        Some(h) => (h, path),
    };
    let key_ok = key == LAST_KEY
        || (!key.is_empty() && key.len() <= 20 && key.bytes().all(|b| b.is_ascii_digit()));
    if !key_ok {
        return None;
    }
    match rest {
        "" => Some((key.to_string(), AssetKind::Image)),
        "body" => Some((key.to_string(), AssetKind::Body)),
        _ => None,
    }
}

/// Which webview may load which preview entry: the preview window loads the pending requests
/// (`preview://<id>`), the settings window (`main`) loads `preview://last`. Everything else
/// (overlays, unknown labels) gets a 404.
pub fn window_may_load(webview_label: &str, key: &str) -> bool {
    match webview_label {
        "preview" => key != LAST_KEY,
        "main" => key == LAST_KEY,
        _ => false,
    }
}

// ---------------------------------------------------------------- DTO projections

fn locality_dto(l: Locality) -> LocalityDto {
    match l {
        Locality::Local => LocalityDto::Local,
        Locality::Cloud => LocalityDto::Cloud,
    }
}

fn urls(key: &str, p: &RequestPreview) -> (Option<String>, String) {
    (
        p.image_png.as_ref().map(|_| format!("preview://{key}")),
        format!("preview://{key}/body"),
    )
}

/// The panel's view of `p` (exact text content; image and raw body via `preview://`).
pub fn preview_dto(request_id: u64, p: &RequestPreview, timeout: Duration) -> PreviewDto {
    let (image_url, body_url) = urls(&request_id.to_string(), p);
    PreviewDto {
        request_id,
        provider_id: p.provider_id.to_string(),
        model: p.model.to_string(),
        locality: locality_dto(p.locality),
        digest_hex: p.digest_hex(),
        transcript: p.transcript.to_string(),
        screen_text: p.screen_text.as_ref().map(|t| t.to_string()),
        image_url,
        body_url,
        secure_fields_hidden: p.secure_fields_hidden,
        timeout_secs: u32::try_from(timeout.as_secs()).unwrap_or(u32::MAX),
    }
}

/// The settings window's "Last request" view of `p`.
pub fn last_request_dto(turn_id: u32, p: &RequestPreview) -> LastRequestDto {
    let (image_url, body_url) = urls(LAST_KEY, p);
    LastRequestDto {
        turn_id,
        provider_id: p.provider_id.to_string(),
        model: p.model.to_string(),
        locality: locality_dto(p.locality),
        digest_hex: p.digest_hex(),
        transcript: p.transcript.to_string(),
        screen_text: p.screen_text.as_ref().map(|t| t.to_string()),
        image_url,
        body_url,
        secure_fields_hidden: p.secure_fields_hidden,
    }
}

// ---------------------------------------------------------------- the gate

struct Pending {
    tx: oneshot::Sender<Decision>,
    /// What the panel was told to show (for `pending_preview`).
    dto: PreviewDto,
    /// When it was shown, so a late pull reports the time left, not the full timeout.
    shown_at: tokio::time::Instant,
}

pub struct ConsentGate {
    pending: Mutex<HashMap<u64, Pending>>,
    sink: Mutex<Arc<dyn PreviewSink>>,
    store: Arc<PreviewStore>,
    next_id: AtomicU64,
    /// Fail closed: starts `true`; `Buddy` mirrors the setting into it.
    local_only: AtomicBool,
    timeout: Duration,
}

impl ConsentGate {
    pub fn new(store: Arc<PreviewStore>) -> Arc<Self> {
        Self::with_timeout(store, CONSENT_TIMEOUT)
    }

    pub fn with_timeout(store: Arc<PreviewStore>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            sink: Mutex::new(Arc::new(NoSink)),
            store,
            next_id: AtomicU64::new(0),
            local_only: AtomicBool::new(true),
            timeout,
        })
    }

    pub fn set_sink(&self, sink: Arc<dyn PreviewSink>) {
        *lock(&self.sink) = sink;
    }

    fn sink(&self) -> Arc<dyn PreviewSink> {
        lock(&self.sink).clone()
    }

    /// Mirror of `Settings::local_only`. Turning it on cancels everything pending.
    pub fn set_local_only(&self, on: bool) {
        self.local_only.store(on, Ordering::SeqCst);
        if on {
            self.cancel_all();
        }
    }

    pub fn local_only(&self) -> bool {
        self.local_only.load(Ordering::SeqCst)
    }

    /// Register `prepared` as pending and show it. Any earlier pending request is cancelled
    /// (one preview at a time; a new turn supersedes the old one). `turn_id` only labels the
    /// request for the caller; supersession is by recency.
    pub fn request_consent(
        self: &Arc<Self>,
        prepared: &PreparedRequest,
        _turn_id: u32,
    ) -> Result<PendingConsent, ConsentError> {
        if self.local_only() {
            return Err(ConsentError::LocalOnly);
        }
        if prepared.locality() != Locality::Cloud {
            return Err(ConsentError::NotCloud);
        }
        self.cancel_all();
        let request_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let preview = prepared.preview();
        let shown_digest = preview.digest;
        let dto = preview_dto(request_id, &preview, self.timeout);
        let (tx, rx) = oneshot::channel();
        self.store.put(&request_id.to_string(), &preview);
        lock(&self.pending).insert(
            request_id,
            Pending {
                tx,
                dto: dto.clone(),
                shown_at: tokio::time::Instant::now(),
            },
        );
        let pending = PendingConsent {
            gate: Arc::clone(self),
            request_id,
            rx,
            prepared: prepared.clone(),
            shown_digest,
        };
        if !self.sink().show(dto) {
            // `pending` is dropped here: entry, store and panel are cleaned up.
            return Err(ConsentError::UiUnavailable);
        }
        Ok(pending)
    }

    /// The user's answer. Single use; unknown/expired/decided/superseded ids are an error.
    pub fn decide(&self, request_id: u64, decision: Decision) -> Result<(), ConsentError> {
        let entry = lock(&self.pending)
            .remove(&request_id)
            .ok_or(ConsentError::UnknownRequest)?;
        self.store.remove(&request_id.to_string());
        self.sink().close(request_id);
        if decision == Decision::Send && self.local_only() {
            // Dropping `entry.tx` makes the waiter see Cancel.
            return Err(ConsentError::LocalOnly);
        }
        entry
            .tx
            .send(decision)
            .map_err(|_| ConsentError::UnknownRequest)
    }

    /// Cancel every pending request (turn cancel, new turn, `local_only` switched on, quit).
    pub fn cancel_all(&self) {
        let ids: Vec<u64> = lock(&self.pending).drain().map(|(id, _)| id).collect();
        for id in ids {
            self.store.remove(&id.to_string());
            self.sink().close(id);
        }
    }

    /// The undecided request the panel should be showing (the newest), for a panel that
    /// mounts after `turn:preview` was emitted. Same data as the event.
    pub fn pending_preview(&self) -> Option<PreviewDto> {
        lock(&self.pending)
            .iter()
            .max_by_key(|(id, _)| **id)
            .map(|(_, p)| {
                // `timeout_secs` is "seconds until the core cancels it": on a late pull that
                // is what is left, so the panel's countdown matches the real deadline.
                let mut dto = p.dto.clone();
                let left = self.timeout.saturating_sub(p.shown_at.elapsed());
                dto.timeout_secs = u32::try_from(left.as_secs()).unwrap_or(u32::MAX);
                dto
            })
    }

    /// Number of undecided requests.
    pub fn pending_count(&self) -> usize {
        lock(&self.pending).len()
    }

    /// Drop a request that was not decided (timeout, waiter gone).
    fn abandon(&self, request_id: u64) {
        if lock(&self.pending).remove(&request_id).is_some() {
            self.store.remove(&request_id.to_string());
            self.sink().close(request_id);
        }
    }
}

/// A registered, not yet decided request. Dropping it cancels the request.
pub struct PendingConsent {
    gate: Arc<ConsentGate>,
    request_id: u64,
    rx: oneshot::Receiver<Decision>,
    prepared: PreparedRequest,
    shown_digest: [u8; 32],
}

impl PendingConsent {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Wait for the decision. `Send` yields the opt-in bound to exactly the request that was
    /// shown; everything else (Cancel, timeout, superseded, quit) is an error.
    pub async fn wait(mut self) -> Result<CloudOptIn, ConsentError> {
        let timeout = self.gate.timeout;
        let decision = match tokio::time::timeout(timeout, &mut self.rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => return Err(ConsentError::Cancelled),
            Err(_) => return Err(ConsentError::TimedOut),
        };
        if decision != Decision::Send {
            return Err(ConsentError::Cancelled);
        }
        if self.gate.local_only() {
            return Err(ConsentError::LocalOnly);
        }
        // The user saw this digest; the request we hold must still be that request.
        if self.prepared.digest() != self.shown_digest {
            return Err(ConsentError::DigestMismatch);
        }
        Ok(CloudOptIn::for_request(
            &self.prepared,
            ConsentSource::PreviewSend {
                request_id: self.request_id,
            },
        ))
    }
}

impl Drop for PendingConsent {
    fn drop(&mut self) {
        self.gate.abandon(self.request_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SettingsPatch;
    use crate::buddy::Buddy;
    use llm::fakes::{FakeCredentialStore, FakeLlm, FakeTransport};
    use llm::{
        AnthropicConfig, AnthropicProvider, LlmError, LlmProvider, LlmRequest, ScreenContext,
        ANTHROPIC_ACCOUNT,
    };
    use sha2::{Digest, Sha256};

    mod testutil {
        use std::time::Instant;

        use firewall::fake::{FakeDetector, FakeSessionProbe, FakeTextSource};
        use firewall::{
            Firewall, NodeRole, PrivacyFirewall, RawTextNode, RawWindowTree, RedactedFrame,
            RedactedScreenText, SessionState, TextVerdict, Verdict,
        };
        use frame::{AppDenyPolicy, DisplayId, DisplayInfo, Frame, Rect};

        pub fn clear_frame(w: u32, h: u32) -> RedactedFrame {
            let d = DisplayInfo {
                id: DisplayId(7),
                origin: (0, 0),
                size: (w, h),
                scale: 1.0,
            };
            let fw = PrivacyFirewall::new(FakeDetector::new(vec![]), FakeSessionProbe::normal());
            assert_eq!(fw.preflight(), SessionState::Active);
            let mut px = Vec::new();
            let mut s: u32 = 7;
            for _ in 0..(w * h) {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let b = (s >> 24) as u8 | 0x40;
                px.extend_from_slice(&[b, b | 0x41, b | 0x42, 255]);
            }
            let f = Frame::new(px, w, h, w * 4, d, Instant::now(), vec![]).unwrap();
            match fw.check(f) {
                Verdict::Clear(rf) => rf,
                other => panic!("expected Clear, got {other:?}"),
            }
        }

        struct DenyNone;
        impl AppDenyPolicy for DenyNone {
            fn is_denied(&self, _: &str) -> bool {
                false
            }
        }

        pub fn screen_text(static_value: &str) -> RedactedScreenText {
            let win = Rect::new(100, 100, 800, 600);
            let pw = Rect::new(200, 260, 300, 30);
            let node = |depth, role, label: Option<&str>, value: Option<&str>| RawTextNode {
                depth,
                role,
                secure: false,
                editable: false,
                label: label.map(String::from),
                value: value.map(String::from),
                has_value: false,
                rect: Some(Rect::new(110, 110, 300, 40)),
            };
            let tree = RawWindowTree {
                app_id: "com.example.app".into(),
                window_rect: win,
                nodes: vec![
                    RawTextNode {
                        rect: Some(win),
                        ..node(0, NodeRole::Window, Some("TITLE"), None)
                    },
                    node(1, NodeRole::StaticText, None, Some(static_value)),
                    node(1, NodeRole::Button, Some("Sign in"), None),
                    RawTextNode {
                        secure: true,
                        editable: true,
                        rect: Some(pw),
                        ..node(1, NodeRole::TextField, Some("Password"), None)
                    },
                ],
            };
            let fw = PrivacyFirewall::new(FakeDetector::new(vec![pw]), FakeSessionProbe::normal())
                .with_text_source(FakeTextSource::new(tree));
            assert_eq!(fw.preflight(), SessionState::Active);
            match fw.check_text(&DenyNone) {
                TextVerdict::Clear(t) => t,
                other => panic!("expected Clear, got {other:?}"),
            }
        }
    }

    /// Plays the preview window: records what it was shown, and fetches the image and raw body
    /// from the store at show time exactly like the `preview://` handler would.
    #[derive(Default)]
    struct TestSink {
        store: Mutex<Option<Arc<PreviewStore>>>,
        shown: Mutex<Vec<Shown>>,
        closed: Mutex<Vec<u64>>,
        refuse: AtomicBool,
    }

    struct Shown {
        dto: PreviewDto,
        image_arc: Option<Arc<[u8]>>,
        image_served: Option<Asset>,
        body_served: Option<Asset>,
    }

    impl PreviewSink for TestSink {
        fn show(&self, dto: PreviewDto) -> bool {
            if self.refuse.load(Ordering::SeqCst) {
                return false;
            }
            let store = lock(&self.store).clone().unwrap();
            let key = dto.request_id.to_string();
            let image_served = dto
                .image_url
                .as_ref()
                .and_then(|u| store.lookup(Some(u.strip_prefix("preview://").unwrap()), ""));
            let body_host = dto.body_url.strip_prefix("preview://").unwrap();
            let (h, p) = body_host.split_once('/').unwrap();
            let body_served = store.lookup(Some(h), p);
            lock(&self.shown).push(Shown {
                image_arc: store.image_arc(&key),
                image_served,
                body_served,
                dto,
            });
            true
        }
        fn close(&self, request_id: u64) {
            lock(&self.closed).push(request_id);
        }
    }

    impl TestSink {
        fn shown_count(&self) -> usize {
            lock(&self.shown).len()
        }
        fn last_id(&self) -> u64 {
            lock(&self.shown).last().unwrap().dto.request_id
        }
    }

    const SYSTEM: &str = "You are a test buddy.";
    const MARKER: &str = "PRIVATE_MARKER_123";

    struct Rig {
        _dir: tempfile::TempDir,
        buddy: Buddy,
        sink: Arc<TestSink>,
        transport: Arc<FakeTransport>,
        creds: Arc<FakeCredentialStore>,
        cloud: AnthropicProvider,
    }

    fn rig(local_only: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let creds = Arc::new(FakeCredentialStore::with_key(ANTHROPIC_ACCOUNT, "k-test"));
        let buddy = Buddy::with_credentials(
            dir.path().join("s.json"),
            Box::new(capture::UnsupportedCapture),
            creds.clone(),
        );
        buddy
            .update_settings(&SettingsPatch {
                local_only: Some(local_only),
                ..Default::default()
            })
            .unwrap();
        let sink = Arc::new(TestSink::default());
        *lock(&sink.store) = Some(buddy.preview_store());
        buddy.set_preview_sink(sink.clone());
        let transport = Arc::new(FakeTransport::responding(200, vec![]));
        let cloud = AnthropicProvider::with_transport(
            AnthropicConfig::new("test-model"),
            transport.clone(),
            creds.clone(),
        )
        .unwrap();
        Rig {
            _dir: dir,
            buddy,
            sink,
            transport,
            creds,
            cloud,
        }
    }

    fn text_request(transcript: &str) -> LlmRequest {
        LlmRequest::new(
            SYSTEM.into(),
            transcript.into(),
            ScreenContext::Text(testutil::screen_text(MARKER)),
        )
        .unwrap()
    }

    fn image_request() -> LlmRequest {
        LlmRequest::new(
            SYSTEM.into(),
            "what is this".into(),
            ScreenContext::TextAndImage {
                text: Some(testutil::screen_text(MARKER)),
                image: testutil::clear_frame(64, 48),
            },
        )
        .unwrap()
    }

    /// Run the policy while a "user" answers with `decision` (or never, for `None`).
    async fn run(
        r: &Rig,
        req: &LlmRequest,
        decision: Option<Decision>,
    ) -> Result<(PreparedRequest, Option<CloudOptIn>), CoreError> {
        let ui = async {
            let Some(d) = decision else { return };
            while r.sink.shown_count() == 0 {
                tokio::task::yield_now().await;
            }
            r.buddy.preview_decision(r.sink.last_id(), d).unwrap();
        };
        let main = r.buddy.prepare_and_consent(&r.cloud, req);
        tokio::pin!(main, ui);
        tokio::select! {
            res = &mut main => res,
            () = &mut ui => main.await,
        }
    }

    /// Whatever happened, a cloud stream without a valid opt-in must not reach the transport.
    async fn assert_no_transport_call(r: &Rig, req: &LlmRequest) {
        let prepared = r.cloud.prepare(req).unwrap();
        let err = r.cloud.stream(prepared, None).await.err().unwrap();
        assert_eq!(err, LlmError::NotOptedIn);
        assert_eq!(r.transport.call_count(), 0);
    }

    fn reference_digest(p: &PreparedRequest) -> [u8; 32] {
        let mut v = b"hey_buddy/llm/v1".to_vec();
        for part in [
            p.provider_id().as_bytes(),
            p.endpoint().as_bytes(),
            p.model().as_bytes(),
            p.body(),
        ] {
            v.extend((part.len() as u64).to_le_bytes());
            v.extend_from_slice(part);
        }
        Sha256::digest(&v).into()
    }

    fn hex(b: [u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_cancels_and_sends_nothing() {
        let r = rig(false);
        let req = text_request("hello");
        let res = run(&r, &req, None).await;
        assert_eq!(res.err(), Some(CoreError::Cancelled));
        assert_eq!(r.sink.shown_count(), 1);
        // The panel was told to close, the store entry is gone, a late decision is an error.
        let id = r.sink.last_id();
        assert_eq!(*lock(&r.sink.closed), vec![id]);
        assert!(!r.buddy.preview_store().contains(&id.to_string()));
        assert_eq!(
            r.buddy.preview_decision(id, Decision::Send),
            Err(ConsentError::UnknownRequest)
        );
        assert_no_transport_call(&r, &req).await;
    }

    #[tokio::test]
    async fn cancel_cancels_and_sends_nothing() {
        let r = rig(false);
        let req = text_request("hello");
        let res = run(&r, &req, Some(Decision::Cancel)).await;
        assert_eq!(res.err(), Some(CoreError::Cancelled));
        assert_no_transport_call(&r, &req).await;
        assert_eq!(r.buddy.consent.pending_count(), 0);
    }

    #[tokio::test]
    async fn no_sink_means_cancelled() {
        let r = rig(false);
        r.sink.refuse.store(true, Ordering::SeqCst);
        let req = text_request("hello");
        let res = run(&r, &req, None).await;
        assert_eq!(res.err(), Some(CoreError::Cancelled));
        assert_eq!(r.buddy.consent.pending_count(), 0);
        assert_no_transport_call(&r, &req).await;
        // And the default sink (nothing installed) behaves the same.
        r.buddy.set_preview_sink(Arc::new(NoSink));
        assert_eq!(run(&r, &req, None).await.err(), Some(CoreError::Cancelled));
    }

    #[tokio::test]
    async fn local_only_mints_nothing_and_shows_nothing() {
        let r = rig(true);
        let req = text_request("hello");
        let res = run(&r, &req, Some(Decision::Send)).await;
        assert_eq!(res.err(), Some(CoreError::LocalOnly));
        assert_eq!(r.sink.shown_count(), 0);
        assert_no_transport_call(&r, &req).await;
        // Directly at the gate too.
        let prepared = r.cloud.prepare(&req).unwrap();
        assert_eq!(
            r.buddy.consent.request_consent(&prepared, 1).err(),
            Some(ConsentError::LocalOnly)
        );
        // A cloud fake provider is refused before anything is prepared; zero streams.
        let fake = FakeLlm::cloud(vec![]);
        let res = r.buddy.prepare_and_consent(&fake, &req).await;
        assert_eq!(res.err(), Some(CoreError::LocalOnly));
        assert_eq!(fake.stream_count(), 0);
        // A fresh gate is local-only until told otherwise (fail closed).
        let gate = ConsentGate::new(Arc::new(PreviewStore::new()));
        assert!(gate.local_only());
    }

    #[tokio::test]
    async fn switching_local_only_on_cancels_pending() {
        let r = rig(false);
        let req = text_request("hello");
        let prepared = r.cloud.prepare(&req).unwrap();
        let pending = r.buddy.consent.request_consent(&prepared, 1).unwrap();
        let id = pending.request_id();
        r.buddy
            .update_settings(&SettingsPatch {
                local_only: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            r.buddy.preview_decision(id, Decision::Send),
            Err(ConsentError::UnknownRequest)
        );
        assert_eq!(pending.wait().await.err(), Some(ConsentError::Cancelled));
    }

    #[tokio::test]
    async fn unknown_and_duplicate_decisions_are_errors() {
        let r = rig(false);
        assert_eq!(
            r.buddy.preview_decision(999, Decision::Send),
            Err(ConsentError::UnknownRequest)
        );
        let req = text_request("hello");
        let prepared = r.cloud.prepare(&req).unwrap();
        let pending = r.buddy.consent.request_consent(&prepared, 1).unwrap();
        let id = pending.request_id();
        assert_eq!(r.buddy.preview_decision(id, Decision::Send), Ok(()));
        assert_eq!(
            r.buddy.preview_decision(id, Decision::Send),
            Err(ConsentError::UnknownRequest)
        );
        assert_eq!(
            r.buddy.preview_decision(id, Decision::Cancel),
            Err(ConsentError::UnknownRequest)
        );
        // The first Send still stands, exactly once.
        assert!(pending.wait().await.is_ok());
    }

    #[tokio::test]
    async fn decision_after_a_newer_turn_is_an_error() {
        let r = rig(false);
        let a = r.cloud.prepare(&text_request("first")).unwrap();
        let b = r.cloud.prepare(&text_request("second")).unwrap();
        let pa = r.buddy.consent.request_consent(&a, 1).unwrap();
        let pb = r.buddy.consent.request_consent(&b, 2).unwrap();
        let (ida, idb) = (pa.request_id(), pb.request_id());
        assert_ne!(ida, idb);
        assert_eq!(
            r.buddy.preview_decision(ida, Decision::Send),
            Err(ConsentError::UnknownRequest)
        );
        assert_eq!(pa.wait().await.err(), Some(ConsentError::Cancelled));
        assert_eq!(r.buddy.preview_decision(idb, Decision::Send), Ok(()));
        assert!(pb.wait().await.is_ok());

        // (A hotkey press during a turn cancels a pending preview too: tests/e2e.rs.)
        // cancel_turn too.
        let d = r.cloud.prepare(&text_request("fourth")).unwrap();
        let pd = r.buddy.consent.request_consent(&d, 4).unwrap();
        r.buddy.cancel_turn();
        assert_eq!(pd.wait().await.err(), Some(ConsentError::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_pull_reports_the_time_left_not_the_full_timeout() {
        let r = rig(false);
        let d = r.cloud.prepare(&text_request("late")).unwrap();
        let _pd = r.buddy.consent.request_consent(&d, 1).unwrap();
        assert_eq!(r.buddy.pending_preview().unwrap().timeout_secs, 120);
        tokio::time::advance(Duration::from_secs(50)).await;
        assert_eq!(r.buddy.pending_preview().unwrap().timeout_secs, 70);
    }

    #[tokio::test]
    async fn send_makes_exactly_one_call_with_the_previewed_bytes() {
        let r = rig(false);
        let req = image_request();
        let (prepared, opt_in) = run(&r, &req, Some(Decision::Send)).await.unwrap();
        let opt_in = opt_in.expect("cloud + Send gives an opt-in");
        assert_eq!(r.transport.call_count(), 0, "nothing sent before streaming");

        // What the "UI" was shown.
        let (id, body_at_show) = {
            let shown = lock(&r.sink.shown);
            let s = &shown[0];
            let preview = prepared.preview();
            assert_eq!(s.dto.digest_hex, hex(reference_digest(&prepared)));
            assert_eq!(s.dto.digest_hex, preview.digest_hex());
            assert_eq!(s.dto.transcript, "what is this");
            assert!(s.dto.screen_text.as_ref().unwrap().contains(MARKER));
            assert!(s.dto.screen_text.as_ref().unwrap().contains("[hidden]"));
            assert_eq!(s.dto.locality, LocalityDto::Cloud);
            assert_eq!(s.dto.secure_fields_hidden, 1);
            assert_eq!(s.dto.timeout_secs, 120);
            let id = s.dto.request_id;
            assert_eq!(
                s.dto.image_url.as_deref(),
                Some(&*format!("preview://{id}"))
            );
            assert_eq!(s.dto.body_url, format!("preview://{id}/body"));
            // The image served is the request's own Arc, byte for byte.
            let png = preview.image_png.as_ref().unwrap();
            assert!(Arc::ptr_eq(s.image_arc.as_ref().unwrap(), png));
            let served = s.image_served.as_ref().unwrap();
            assert_eq!(served.content_type, "image/png");
            assert_eq!(&served.bytes[..], &png[..]);
            assert_eq!(served.bytes.as_ptr(), png.as_ptr());
            // The raw body served is the exact body.
            let body = s.body_served.as_ref().unwrap();
            assert_eq!(body.content_type, "application/json");
            assert_eq!(&body.bytes, prepared.body());
            let body_at_show = body.bytes.clone();
            (id, body_at_show)
        };

        // The entry is gone once decided.
        assert!(!r.buddy.preview_store().contains(&id.to_string()));
        assert!(r
            .buddy
            .preview_store()
            .lookup(Some(&id.to_string()), "")
            .is_none());

        // Send -> exactly one transport call carrying exactly the previewed bytes.
        let _ = r
            .cloud
            .stream(prepared.clone(), Some(opt_in))
            .await
            .unwrap();
        let calls = r.transport.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].body, body_at_show);
        assert_eq!(&calls[0].body, prepared.body());
        assert_eq!(calls[0].url, prepared.endpoint());
        // The key is only ever in a sensitive header.
        assert_eq!(
            calls[0]
                .headers
                .iter()
                .filter(|(_, v, _)| v == "k-test")
                .count(),
            1
        );
        assert!(calls[0]
            .headers
            .iter()
            .all(|(_, v, sens)| v != "k-test" || *sens));
        assert!(r.creds.get_count() >= 1);
    }

    #[tokio::test]
    async fn opt_in_is_bound_to_the_request_it_was_given_for() {
        let r = rig(false);
        let (_a, opt_a) = run(&r, &text_request("request A"), Some(Decision::Send))
            .await
            .unwrap();
        let b = r.cloud.prepare(&text_request("request B")).unwrap();
        let err = r.cloud.stream(b, opt_a).await.err().unwrap();
        assert_eq!(err, LlmError::NotOptedIn);
        assert_eq!(r.transport.call_count(), 0);
    }

    #[tokio::test]
    async fn local_provider_needs_no_consent() {
        let r = rig(true);
        let local = FakeLlm::local(vec![]);
        let req = text_request("hello");
        let (prepared, opt) = r.buddy.prepare_and_consent(&local, &req).await.unwrap();
        assert!(opt.is_none());
        assert_eq!(r.sink.shown_count(), 0);
        assert_eq!(prepared.locality(), Locality::Local);
        assert_eq!(r.buddy.consent.pending_count(), 0);
        // Same with local_only off.
        r.buddy
            .update_settings(&SettingsPatch {
                local_only: Some(false),
                ..Default::default()
            })
            .unwrap();
        let (_, opt) = r.buddy.prepare_and_consent(&local, &req).await.unwrap();
        assert!(opt.is_none());
        assert_eq!(r.sink.shown_count(), 0);
        // The gate itself refuses a local request.
        assert_eq!(
            r.buddy.consent.request_consent(&prepared, 1).err(),
            Some(ConsentError::NotCloud)
        );
    }

    #[tokio::test]
    async fn last_request_is_kept_for_local_and_cloud_and_replaced() {
        let r = rig(false);
        assert!(r.buddy.get_last_request().is_none());
        let local = FakeLlm::local(vec![]);
        let (p, _) = r
            .buddy
            .prepare_and_consent(&local, &image_request())
            .await
            .unwrap();
        let last = r.buddy.get_last_request().unwrap();
        assert_eq!(last.locality, LocalityDto::Local);
        assert_eq!(last.digest_hex, hex(p.digest()));
        assert_eq!(last.image_url.as_deref(), Some("preview://last"));
        assert_eq!(last.body_url, "preview://last/body");
        assert!(last.screen_text.unwrap().contains("[hidden]"));
        let store = r.buddy.preview_store();
        let img = store.lookup(Some("last"), "").unwrap();
        assert_eq!(img.content_type, "image/png");
        assert!(Arc::ptr_eq(
            &store.image_arc("last").unwrap(),
            p.preview().image_png.as_ref().unwrap()
        ));
        // Next turn replaces it (cloud, even when cancelled it is still the last request).
        let _ = run(&r, &text_request("second"), Some(Decision::Cancel)).await;
        let last = r.buddy.get_last_request().unwrap();
        assert_eq!(last.locality, LocalityDto::Cloud);
        assert_eq!(last.transcript, "second");
        assert!(last.image_url.is_none());
        assert!(store.lookup(Some("last"), "").is_none());
        assert_eq!(
            &store.lookup(Some("last"), "/body").unwrap().bytes,
            &r.cloud
                .prepare(&text_request("second"))
                .unwrap()
                .body()
                .clone()
        );
    }

    #[test]
    fn uri_parsing() {
        use AssetKind::*;
        let k = |h, p| parse_uri(h, p);
        assert_eq!(k(Some("12"), ""), Some(("12".into(), Image)));
        assert_eq!(k(Some("12"), "/"), Some(("12".into(), Image)));
        assert_eq!(k(Some("12"), "/body"), Some(("12".into(), Body)));
        assert_eq!(k(Some("last"), "/body"), Some(("last".into(), Body)));
        // Windows form: http://preview.localhost/<key>[/body]
        assert_eq!(
            k(Some("preview.localhost"), "/12"),
            Some(("12".into(), Image))
        );
        assert_eq!(
            k(Some("preview.localhost"), "/12/body"),
            Some(("12".into(), Body))
        );
        assert_eq!(k(None, "/last"), Some(("last".into(), Image)));
        for bad in [
            k(Some("12"), "/body/x"),
            k(Some("12"), "/other"),
            k(Some("abc"), ""),
            k(Some(".."), ""),
            k(Some("1x"), ""),
            k(Some("preview.localhost"), "/"),
            k(Some("preview.localhost"), "/../body"),
            k(Some("123456789012345678901"), ""),
            k(None, ""),
        ] {
            assert_eq!(bad, None);
        }
    }

    #[test]
    fn only_the_right_windows_may_load_previews() {
        assert!(window_may_load("preview", "12"));
        assert!(!window_may_load("preview", "last"));
        assert!(window_may_load("main", "last"));
        assert!(!window_may_load("main", "12"));
        assert!(!window_may_load("overlay-1-0", "12"));
        assert!(!window_may_load("overlay-1-0", "last"));
        assert!(!window_may_load("", "last"));
    }

    #[test]
    fn body_only_entry_has_no_image() {
        let store = PreviewStore::new();
        let r = rig(false);
        let p = r.cloud.prepare(&text_request("x")).unwrap().preview();
        store.put("5", &p);
        assert!(store.asset("5", AssetKind::Image).is_none());
        assert!(store.asset("5", AssetKind::Body).is_some());
        assert!(store.asset("6", AssetKind::Body).is_none());
    }

    /// CI also greps for this; running it here catches a stray use before CI does.
    #[test]
    fn only_this_file_and_llm_mint_opt_ins() {
        fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if p.is_dir() {
                    if !["target", "node_modules", "dist", "gen", ".git"].contains(&name.as_str()) {
                        walk(&p, hits);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    if text.contains("for_request") {
                        hits.push(p.to_string_lossy().to_string());
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut hits = Vec::new();
        for d in ["crates", "apps", "tools"] {
            walk(&root.join(d), &mut hits);
        }
        let stray: Vec<_> = hits
            .into_iter()
            .filter(|h| !h.contains("crates/llm/") && !h.ends_with("crates/core/src/consent.rs"))
            .collect();
        assert!(
            stray.is_empty(),
            "CloudOptIn::for_request used in {stray:?}"
        );
    }
}
