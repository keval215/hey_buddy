//! Launch allowlist, rate limiter and the app deny snapshot.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use action::LaunchId;
use frame::AppDenyPolicy;

/// Minimum time between two executes.
pub const MIN_EXEC_GAP: Duration = Duration::from_millis(250);
/// Maximum executes in any rolling minute.
pub const MAX_EXECS_PER_MINUTE: usize = 30;
/// A cached observation older than this is stale (>= ADR 0003's approval timeout).
pub const OBSERVATION_MAX_AGE: Duration = Duration::from_secs(60);
/// A single actuator call (or revalidation) that makes no progress for this long is a timeout.
pub const CALL_DEADLINE: Duration = Duration::from_secs(3);

/// `LaunchId` -> fixed executable path. No arguments, verbs or URIs are ever passed.
#[derive(Clone, Default)]
pub struct LaunchAllowlist {
    entries: Vec<(LaunchId, PathBuf)>,
}

impl LaunchAllowlist {
    /// Nothing may be launched.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Adds (or replaces) an entry. `None` when the id is not a valid `LaunchId` or the path is
    /// empty.
    pub fn with(mut self, id: &str, path: impl Into<PathBuf>) -> Option<Self> {
        let id = LaunchId::new(id).ok()?;
        let path = path.into();
        if path.as_os_str().is_empty() {
            return None;
        }
        self.entries.retain(|(i, _)| *i != id);
        self.entries.push((id, path));
        Some(self)
    }

    /// The default list: `notepad` -> `%SystemRoot%\System32\notepad.exe` (ADR 0004 OQ5).
    pub fn windows_default() -> Self {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        Self::empty()
            .with("notepad", format!("{root}\\System32\\notepad.exe"))
            .unwrap_or_default()
    }

    pub fn resolve(&self, id: &LaunchId) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, p)| p.as_path())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for LaunchAllowlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LaunchAllowlist({} entries)", self.entries.len())
    }
}

/// Rolling-minute rate limiter with a 250 ms floor. Time comes from the injected clock.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    last: Option<Duration>,
    recent: VecDeque<Duration>,
}

impl Limiter {
    /// Records an execute at `now` or says it is over the limit (never queued). A refused call
    /// is not recorded.
    pub(crate) fn admit(&mut self, now: Duration) -> bool {
        if let Some(last) = self.last {
            if now < last || now - last < MIN_EXEC_GAP {
                return false;
            }
        }
        while let Some(&t) = self.recent.front() {
            if now.saturating_sub(t) >= Duration::from_secs(60) {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.recent.len() >= MAX_EXECS_PER_MINUTE {
            return false;
        }
        self.last = Some(now);
        self.recent.push_back(now);
        true
    }
}

/// The deny policy evaluated ahead of time on the caller thread for every app id the execute can
/// ask about, so it can travel to the worker thread (`&dyn AppDenyPolicy` is not `'static`).
/// An id that was not asked about is DENIED (fail closed).
pub(crate) struct DenySnapshot {
    verdicts: HashMap<String, bool>,
}

impl DenySnapshot {
    pub(crate) fn take<'a>(
        deny: &dyn AppDenyPolicy,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut verdicts = HashMap::new();
        for id in ids {
            let k = id.trim().to_ascii_lowercase();
            let v = deny.is_denied(id);
            let e = verdicts.entry(k).or_insert(false);
            *e |= v;
        }
        Self { verdicts }
    }
}

impl AppDenyPolicy for DenySnapshot {
    fn is_denied(&self, app_id: &str) -> bool {
        self.verdicts
            .get(&app_id.trim().to_ascii_lowercase())
            .copied()
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn floor_and_minute_window() {
        let mut l = Limiter::default();
        assert!(l.admit(ms(1000)));
        assert!(!l.admit(ms(1249)), "249 ms after the last");
        assert!(l.admit(ms(1250)), "exactly 250 ms is allowed");
        let mut t = 1250;
        for _ in 0..28 {
            t += 250;
            assert!(l.admit(ms(t)));
        }
        // 30 admitted so far (all within 60 s).
        t += 250;
        assert!(!l.admit(ms(t)), "31st in a minute");
        assert!(
            !l.admit(ms(t + 1)),
            "a refused call is not recorded but stays refused"
        );
        // The first entry (1000 ms) leaves the window at 61 000 ms.
        assert!(l.admit(ms(61_000)));
    }

    #[test]
    fn clock_going_backwards_is_refused() {
        let mut l = Limiter::default();
        assert!(l.admit(ms(5000)));
        assert!(!l.admit(ms(100)));
    }

    #[test]
    fn deny_snapshot_unknown_is_denied() {
        struct D;
        impl AppDenyPolicy for D {
            fn is_denied(&self, a: &str) -> bool {
                a == "bank.exe"
            }
        }
        let s = DenySnapshot::take(&D, ["Notes.exe", "bank.exe"]);
        assert!(!s.is_denied("notes.exe"));
        assert!(s.is_denied("BANK.exe"));
        assert!(s.is_denied("other.exe"));
    }

    #[test]
    fn allowlist() {
        let a = LaunchAllowlist::empty()
            .with("notepad", "/x/notepad")
            .unwrap();
        assert_eq!(
            a.resolve(&LaunchId::new("notepad").unwrap()),
            Some(Path::new("/x/notepad"))
        );
        assert!(a.resolve(&LaunchId::new("calc").unwrap()).is_none());
        assert!(LaunchAllowlist::empty().with("Bad Id", "/x").is_none());
        assert!(LaunchAllowlist::empty().with("ok", "").is_none());
        assert_eq!(format!("{a:?}"), "LaunchAllowlist(1 entries)");
    }
}
