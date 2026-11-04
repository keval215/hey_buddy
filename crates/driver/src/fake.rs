//! `FakeActuator` (feature `fakes`): records every call and mutates a shared
//! `firewall::fake::ScriptedDesktop` world, so tests run the REAL `PrivacyFirewall` and the REAL
//! `Driver` against it (no fake firewall, ADR 0001). Failure, panic and hang injection and a hook
//! that runs after every call (steal focus, rename a label, lock the session, trip the kill
//! switch, ...). Calls are recorded as kinds and lengths; typed text is never recorded.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use action::{KeyCombo, ScrollAmount, ScrollDirection, TypedText};
use firewall::fake::{ScriptedDesktop, WindowSpec};
use firewall::{AppId, ElementRef, LiveTarget};
use frame::Rect;

use crate::actuator::{Actuator, ClickMethod, MAX_CHUNK_CHARS};
use crate::error::ActError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    Invoke,
    SetValue,
    SetFocus,
    Scroll,
    SendChars,
    SendKey,
    SendCopy,
    Launch,
    FocusApp,
}

/// One recorded actuator call. No text, no labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// 0-based position in the log.
    pub seq: usize,
    pub kind: CallKind,
    /// `SendChars`: characters in the chunk.
    pub chars: Option<usize>,
    /// Target-taking calls: the `#n` of the element (None for the window targets).
    pub element: Option<ElementRef>,
    pub click: Option<ClickMethod>,
    pub key: Option<KeyCombo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inject {
    /// The call returns this error (after being recorded; no effect on the world).
    Fail(ActError),
    /// The call panics.
    Panic,
    /// The call blocks until [`FakeActuator::resume`].
    Hang,
}

type Hook = Arc<dyn Fn(&Call) + Send + Sync>;

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    inject: HashMap<CallKind, Inject>,
    hook: Option<Hook>,
    hanging: bool,
    hang_entered: usize,
    releases: usize,
    focus_lands: bool,
    next_window: u64,
}

struct Inner {
    desk: ScriptedDesktop,
    state: Mutex<State>,
    cv: Condvar,
}

/// Cheap to clone; all clones share one log and one world.
#[derive(Clone)]
pub struct FakeActuator {
    inner: Arc<Inner>,
}

impl FakeActuator {
    pub fn new(desk: ScriptedDesktop) -> Self {
        Self {
            inner: Arc::new(Inner {
                desk,
                state: Mutex::new(State {
                    focus_lands: true,
                    next_window: 0x9000,
                    ..State::default()
                }),
                cv: Condvar::new(),
            }),
        }
    }

    pub fn desk(&self) -> &ScriptedDesktop {
        &self.inner.desk
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- inspection ----

    pub fn calls(&self) -> Vec<Call> {
        self.st().calls.clone()
    }
    pub fn call_count(&self) -> usize {
        self.st().calls.len()
    }
    pub fn count(&self, kind: CallKind) -> usize {
        self.st().calls.iter().filter(|c| c.kind == kind).count()
    }
    /// Total characters passed to `send_chars`.
    pub fn chars_sent(&self) -> usize {
        self.st().calls.iter().filter_map(|c| c.chars).sum()
    }
    /// Lengths of the `send_chars` chunks, in order.
    pub fn chunk_lens(&self) -> Vec<usize> {
        self.st().calls.iter().filter_map(|c| c.chars).collect()
    }
    pub fn clear_calls(&self) {
        self.st().calls.clear();
    }
    /// Times `release_all` was called.
    pub fn releases(&self) -> usize {
        self.st().releases
    }
    /// Calls currently blocked by an [`Inject::Hang`].
    pub fn hang_entries(&self) -> usize {
        self.st().hang_entered
    }

    // ---- scripting ----

    pub fn inject(&self, kind: CallKind, what: Inject) {
        self.st().inject.insert(kind, what);
    }
    pub fn clear_injections(&self) {
        self.st().inject.clear();
    }
    /// Unblock every hung call (and stop hanging).
    pub fn resume(&self) {
        self.st().hanging = false;
        self.inner.cv.notify_all();
    }
    /// Runs after every recorded call, outside the log lock.
    pub fn set_hook(&self, f: impl Fn(&Call) + Send + Sync + 'static) {
        self.st().hook = Some(Arc::new(f));
    }
    pub fn clear_hook(&self) {
        self.st().hook = None;
    }
    /// `false`: `set_focus` returns Ok but focus does not move (a refused SetFocus).
    pub fn set_focus_lands(&self, lands: bool) {
        self.st().focus_lands = lands;
    }

    // ---- internals ----

    /// Records the call, applies injection, runs the hook. `Ok(true)` = apply the effect.
    fn enter(
        &self,
        kind: CallKind,
        chars: Option<usize>,
        live: Option<&LiveTarget<'_>>,
        click: Option<ClickMethod>,
        key: Option<KeyCombo>,
    ) -> Result<Call, ActError> {
        let (call, inject) = {
            let mut s = self.st();
            let call = Call {
                seq: s.calls.len(),
                kind,
                chars,
                element: live.and_then(LiveTarget::element),
                click,
                key,
            };
            s.calls.push(call.clone());
            (call, s.inject.get(&kind).copied())
        };
        match inject {
            Some(Inject::Fail(e)) => return Err(e),
            Some(Inject::Panic) => panic!("fake actuator panic"),
            Some(Inject::Hang) => {
                let mut s = self.st();
                s.hanging = true;
                s.hang_entered += 1;
                while s.hanging {
                    s = self.inner.cv.wait(s).unwrap_or_else(|e| e.into_inner());
                }
            }
            None => {}
        }
        Ok(call)
    }

    fn after(&self, call: &Call) {
        let hook = self.st().hook.clone();
        if let Some(h) = hook {
            h(call);
        }
    }

    /// Runtime id and window of the world element behind a live target.
    fn world_elem(&self, live: &LiveTarget<'_>) -> Option<(u64, Vec<i32>)> {
        let el = live.element()?;
        let rect: Rect = live.observation().describe(el)?.rect;
        let hwnd = self.inner.desk.foreground_hwnd();
        let rid = self.inner.desk.element_at(hwnd, rect)?;
        Some((hwnd, rid))
    }

    fn append_to_focused(&self, f: impl FnOnce(String) -> String) {
        if let Some((hwnd, rid)) = self.inner.desk.focused() {
            let cur = self.inner.desk.value_of(hwnd, &rid).unwrap_or_default();
            let new = f(cur);
            let v = (!new.is_empty()).then_some(new);
            self.inner.desk.set_value(hwnd, &rid, v.as_deref());
        }
    }
}

impl std::fmt::Debug for FakeActuator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FakeActuator({} calls)", self.call_count())
    }
}

impl Actuator for FakeActuator {
    fn invoke(&self, t: &LiveTarget<'_>, m: ClickMethod) -> Result<(), ActError> {
        let c = self.enter(CallKind::Invoke, None, Some(t), Some(m), None)?;
        if m == ClickMethod::Toggle {
            if let Some((h, rid)) = self.world_elem(t) {
                let on = self.inner.desk.value_of(h, &rid).is_some();
                self.inner
                    .desk
                    .set_value(h, &rid, if on { None } else { Some("on") });
            }
        }
        self.after(&c);
        Ok(())
    }

    fn set_value(&self, t: &LiveTarget<'_>, text: &TypedText) -> Result<(), ActError> {
        let c = self.enter(CallKind::SetValue, None, Some(t), None, None)?;
        if let Some((h, rid)) = self.world_elem(t) {
            self.inner.desk.set_value(h, &rid, Some(text.as_str()));
        }
        self.after(&c);
        Ok(())
    }

    fn set_focus(&self, t: &LiveTarget<'_>) -> Result<(), ActError> {
        let c = self.enter(CallKind::SetFocus, None, Some(t), None, None)?;
        let lands = self.st().focus_lands;
        if lands {
            if let Some((h, rid)) = self.world_elem(t) {
                self.inner.desk.set_focus(h, Some(&rid));
            }
        }
        self.after(&c);
        Ok(())
    }

    fn scroll(
        &self,
        t: &LiveTarget<'_>,
        _d: ScrollDirection,
        _a: ScrollAmount,
    ) -> Result<(), ActError> {
        let c = self.enter(CallKind::Scroll, None, Some(t), None, None)?;
        self.after(&c);
        Ok(())
    }

    fn send_chars(&self, chunk: &str) -> Result<(), ActError> {
        let n = chunk.chars().count();
        if n == 0 || n > MAX_CHUNK_CHARS {
            return Err(ActError::Platform("bad chunk"));
        }
        let c = self.enter(CallKind::SendChars, Some(n), None, None, None)?;
        self.append_to_focused(|mut cur| {
            cur.push_str(chunk);
            cur
        });
        self.after(&c);
        Ok(())
    }

    fn send_key(&self, k: KeyCombo) -> Result<(), ActError> {
        let c = self.enter(CallKind::SendKey, None, None, None, Some(k))?;
        if k == KeyCombo::Backspace {
            self.append_to_focused(|mut cur| {
                cur.pop();
                cur
            });
        }
        self.after(&c);
        Ok(())
    }

    fn send_copy(&self) -> Result<(), ActError> {
        let c = self.enter(CallKind::SendCopy, None, None, None, None)?;
        self.after(&c);
        Ok(())
    }

    fn launch(&self, path: &Path) -> Result<(), ActError> {
        let c = self.enter(CallKind::Launch, None, None, None, None)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .or_else(|| path.to_str().and_then(|s| s.rsplit(['\\', '/']).next()))
            .unwrap_or("app.exe")
            .to_lowercase();
        let n = {
            let mut s = self.st();
            s.next_window += 1;
            s.next_window
        };
        self.inner.desk.add_window(WindowSpec::new(
            n,
            &name,
            u32::try_from(n).unwrap_or(1),
            Rect::new(0, 0, 400, 300),
        ));
        self.after(&c);
        Ok(())
    }

    fn focus_app(&self, app: &AppId) -> Result<(), ActError> {
        let c = self.enter(CallKind::FocusApp, None, None, None, None)?;
        if let Some(h) = self.inner.desk.hwnd_of_app(app.as_str()) {
            self.inner.desk.set_foreground(h);
        }
        self.after(&c);
        Ok(())
    }

    fn release_all(&self) {
        self.st().releases += 1;
    }
}
