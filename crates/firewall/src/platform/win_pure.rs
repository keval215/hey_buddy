//! Pure, Linux-testable pieces of the Windows agent adapters (`windows_agent.rs`, S2-14d1):
//! runtime-id handling, SAFEARRAY conversion logic, integrity-level mapping, exe path to app id,
//! process-creation-time comparison, the ancestor-walk bound, rect conversion, desktop-name
//! comparison, the visible-window filter and the resolver read ORDER (`IsPassword` first).
//!
//! Nothing here touches the OS. The Windows module only implements the small traits below on
//! UIA elements, so every privacy-relevant decision is unit-tested on Linux.
// Used by the Windows adapters and tests only.
#![cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]

use frame::Rect;

use crate::observe::AppId;
use crate::target::{hash_automation_id, hash_label, NodeIdentity, ResolvedFields, ResolvedTarget};
use crate::FirewallError;

// ---------- runtime ids ----------

/// Longest runtime id accepted (UIA ids have 2-5 integers in practice).
pub(crate) const MAX_RUNTIME_ID_LEN: usize = 32;

/// A usable runtime id: non-empty and bounded. An empty id would mean "the window" to
/// `TargetResolver::focus_state`, so it must never name an element.
pub(crate) fn canonical_runtime_id(ids: &[i32]) -> Option<Vec<i32>> {
    (!ids.is_empty() && ids.len() <= MAX_RUNTIME_ID_LEN).then(|| ids.to_vec())
}

/// Exact comparison of two runtime ids; both must be usable.
pub(crate) fn runtime_ids_equal(a: &[i32], b: &[i32]) -> bool {
    canonical_runtime_id(a).is_some() && a == b
}

/// A one-dimensional array of `i32` (a `SAFEARRAY` of `VT_I4` on Windows).
pub(crate) trait Int32Array {
    fn lower_bound(&self) -> Option<i32>;
    fn upper_bound(&self) -> Option<i32>;
    fn get(&self, index: i32) -> Option<i32>;
}

/// Copy the array out. `None` when empty, oversized, or any element cannot be read (fail closed:
/// a half-read runtime id would name a different element).
pub(crate) fn read_int32_array<A: Int32Array>(a: &A) -> Option<Vec<i32>> {
    let (lo, hi) = (i64::from(a.lower_bound()?), i64::from(a.upper_bound()?));
    let count = hi.checked_sub(lo)?.checked_add(1)?;
    if count < 1 || count as usize > MAX_RUNTIME_ID_LEN {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in lo..=hi {
        out.push(a.get(i32::try_from(i).ok()?)?);
    }
    canonical_runtime_id(&out)
}

// ---------- integrity ----------

/// Mandatory integrity levels, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IntegrityLevel {
    Untrusted = 0,
    Low = 1,
    Medium = 2,
    MediumPlus = 3,
    High = 4,
    System = 5,
    Protected = 6,
}

impl IntegrityLevel {
    /// Map the last sub-authority (RID) of a token's integrity SID. Values between the
    /// well-known levels round DOWN to the level below; anything at or above 0x5000 is
    /// `Protected` (the most privileged), so an unexpected RID never looks harmless.
    pub(crate) fn from_rid(rid: u32) -> Self {
        match rid {
            0..=0x0FFF => Self::Untrusted,
            0x1000..=0x1FFF => Self::Low,
            0x2000..=0x20FF => Self::Medium,
            0x2100..=0x2FFF => Self::MediumPlus,
            0x3000..=0x3FFF => Self::High,
            0x4000..=0x4FFF => Self::System,
            _ => Self::Protected,
        }
    }

    /// The number stored in `ForegroundFacts::integrity` (higher = more privileged).
    pub(crate) fn rank(self) -> u32 {
        self as u32
    }
}

// ---------- processes ----------

/// FILETIME ticks (100 ns since 1601) from the two halves `GetProcessTimes` returns.
pub(crate) fn filetime_ticks(low: u32, high: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Two reads name the same process instance: equal pid AND equal non-zero creation time. A zero
/// time means "could not be read" and never matches.
pub(crate) fn same_process(a: (u32, u64), b: (u32, u64)) -> bool {
    a.0 != 0 && a.1 != 0 && a == b
}

/// Lowercase basename of an image path, validated as an [`AppId`] (so deny matching lines up
/// with `capture`'s exe resolution). `None` for an empty / invalid name.
pub(crate) fn app_id_from_exe_path(path: &str) -> Option<String> {
    let base = path
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.trim().is_empty())?;
    AppId::parse(base).map(|id| id.as_str().to_string())
}

/// A top-level window belongs in `Observation::apps()`: visible, not cloaked (UWP ghosts, other
/// virtual desktops), not minimised, and not ours. Deny / auth filtering is the firewall core's.
pub(crate) fn include_in_apps(
    visible: bool,
    cloaked: bool,
    minimized: bool,
    pid: u32,
    own_pid: u32,
) -> bool {
    visible && !cloaked && !minimized && pid != 0 && pid != own_pid
}

// ---------- desktop / geometry ----------

/// `GetUserObjectInformationW(UOI_NAME)` of the input desktop is the user's normal desktop
/// ("Default", case-insensitive). `name` may carry trailing NULs.
pub(crate) fn is_user_desktop_name(name: &[u16]) -> bool {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let n = &name[..end];
    const DEFAULT: &[u8] = b"default";
    n.len() == DEFAULT.len()
        && n.iter()
            .zip(DEFAULT)
            .all(|(&c, &d)| u8::try_from(c).is_ok_and(|c| c.to_ascii_lowercase() == d))
}

/// A UIA bounding rectangle (left, top, right, bottom) as a [`Rect`]. `None` for negative or
/// overflowing sizes.
pub(crate) fn rect_from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Option<Rect> {
    let w = u32::try_from(i64::from(right) - i64::from(left)).ok()?;
    let h = u32::try_from(i64::from(bottom) - i64::from(top)).ok()?;
    Some(Rect::new(left, top, w, h))
}

/// Exact rect comparison (v1: no tolerance, ADR 0004 §6). The firewall core compares the
/// resolver's rect with `==`; this documents (and tests) the rule for adapters.
#[allow(dead_code)]
pub(crate) fn rects_equal(a: &Rect, b: &Rect) -> bool {
    a == b
}

// ---------- focus ----------

/// Bound of the parent walk used to decide "the focused element is inside the target".
#[allow(dead_code)] // used by the Windows resolver
pub(crate) const MAX_ANCESTOR_STEPS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AncestorOutcome {
    /// Some proper ancestor of the start is the target.
    Found,
    /// The chain ended (root reached) without meeting the target.
    NotFound,
    /// More than `max_steps` ancestors: treated as "not inside" by callers (fail closed).
    BoundExceeded,
}

/// Walk the parents of `start` (not `start` itself) looking for the target, at most `max_steps`
/// steps. Errors from the platform abort the walk.
pub(crate) fn find_ancestor<N, E>(
    start: &N,
    max_steps: usize,
    mut parent: impl FnMut(&N) -> Result<Option<N>, E>,
    mut is_target: impl FnMut(&N) -> Result<bool, E>,
) -> Result<AncestorOutcome, E> {
    // The first parent is computed from `start`; later ones from the previous parent.
    let mut cur = match parent(start)? {
        Some(p) => p,
        None => return Ok(AncestorOutcome::NotFound),
    };
    for _ in 0..max_steps {
        if is_target(&cur)? {
            return Ok(AncestorOutcome::Found);
        }
        cur = match parent(&cur)? {
            Some(p) => p,
            None => return Ok(AncestorOutcome::NotFound),
        };
    }
    Ok(AncestorOutcome::BoundExceeded)
}

/// Controls whose `Name` the resolver may read for the focus-sensitivity check: only text-like
/// controls (Edit, ComboBox and other editable types). Never documents, panes, groups or windows:
/// their names are page / window titles (D-003) and the check must not read them.
pub(crate) fn focus_label_check_needed(control_type: i32) -> bool {
    super::role_map::uia_is_editable(control_type)
        || matches!(
            super::role_map::uia_role(control_type),
            crate::NodeRole::TextField | crate::NodeRole::ComboBox
        )
}

// ---------- identity and resolution ----------

/// Identity of a non-secure node from raw reads. The automation id is hashed here and dropped;
/// `label_hash` stays zeroed (the walker fills it from the label it already read). `None` when
/// the runtime id or control type is unusable: the node is then simply not addressable.
pub(crate) fn node_identity(
    runtime_id: &[i32],
    control_type: i32,
    automation_id: &str,
) -> Option<NodeIdentity> {
    Some(NodeIdentity {
        runtime_id: canonical_runtime_id(runtime_id)?,
        control_type: u32::try_from(control_type).ok()?,
        automation_id_hash: hash_automation_id(automation_id),
        label_hash: [0; 32],
    })
}

/// The reads `revalidate` performs on a re-resolved element, one attribute per method, so the
/// ORDER is testable. The Windows implementation wraps an `IUIAutomationElement`.
pub(crate) trait ElementReads {
    fn is_password(&mut self) -> Result<bool, FirewallError>;
    fn control_type(&mut self) -> Result<i32, FirewallError>;
    fn automation_id(&mut self) -> Result<String, FirewallError>;
    /// The element's `Name`. The caller hashes it immediately and never returns it.
    fn name(&mut self) -> Result<String, FirewallError>;
    fn rect(&mut self) -> Result<Rect, FirewallError>;
}

/// `IsPassword` FIRST; a secure element yields `Secure` and NOTHING else is read.
pub(crate) fn read_resolved<E: ElementReads>(e: &mut E) -> Result<ResolvedTarget, FirewallError> {
    if e.is_password()? {
        return Ok(ResolvedTarget::Secure);
    }
    let control_type = u32::try_from(e.control_type()?).map_err(|_| FirewallError::Detection)?;
    let automation_id_hash = hash_automation_id(&e.automation_id()?);
    let label_hash = hash_label(Some(&e.name()?));
    let rect = e.rect()?;
    Ok(ResolvedTarget::Found(ResolvedFields {
        control_type,
        automation_id_hash,
        label_hash,
        rect,
    }))
}

/// Focus decision for the focused element once it has been located: `IsPassword` first, then (for
/// text-like controls only) the secret-lexicon check on the name just read.
pub(crate) trait FocusedReads {
    fn is_password(&mut self) -> Result<bool, FirewallError>;
    fn control_type(&mut self) -> Result<i32, FirewallError>;
    fn name(&mut self) -> Result<String, FirewallError>;
}

/// `Ok(Some(state))` when the focused element alone decides (`Secure` / `Sensitive`);
/// `Ok(None)` when it is neither (the caller then compares it with the target).
pub(crate) fn classify_focused<F: FocusedReads>(
    f: &mut F,
) -> Result<Option<crate::target::FocusState>, FirewallError> {
    use crate::target::FocusState;
    if f.is_password()? {
        return Ok(Some(FocusState::Secure));
    }
    if focus_label_check_needed(f.control_type()?) && crate::sensitive::sensitive_label(&f.name()?)
    {
        return Ok(Some(FocusState::Sensitive));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::FocusState;

    struct Arr {
        lo: Option<i32>,
        data: Vec<Option<i32>>,
    }
    impl Int32Array for Arr {
        fn lower_bound(&self) -> Option<i32> {
            self.lo
        }
        fn upper_bound(&self) -> Option<i32> {
            self.lo.map(|l| l + self.data.len() as i32 - 1)
        }
        fn get(&self, i: i32) -> Option<i32> {
            self.data
                .get(usize::try_from(i - self.lo?).ok()?)
                .copied()
                .flatten()
        }
    }

    #[test]
    fn runtime_ids_must_be_non_empty_bounded_and_equal() {
        assert_eq!(canonical_runtime_id(&[42, 7]), Some(vec![42, 7]));
        assert_eq!(canonical_runtime_id(&[]), None);
        assert_eq!(canonical_runtime_id(&[1; MAX_RUNTIME_ID_LEN + 1]), None);
        assert!(canonical_runtime_id(&[1; MAX_RUNTIME_ID_LEN]).is_some());
        assert!(runtime_ids_equal(&[42, 7], &[42, 7]));
        assert!(!runtime_ids_equal(&[42, 7], &[42, 8]));
        assert!(!runtime_ids_equal(&[42], &[42, 7]));
        assert!(!runtime_ids_equal(&[], &[]), "empty ids never match");
    }

    #[test]
    fn safearray_conversion_handles_bounds_and_failures() {
        let ok = Arr {
            lo: Some(0),
            data: vec![Some(42), Some(-3), Some(9)],
        };
        assert_eq!(read_int32_array(&ok), Some(vec![42, -3, 9]));
        // Non-zero lower bound (SAFEARRAYs may start anywhere).
        let shifted = Arr {
            lo: Some(1),
            data: vec![Some(5), Some(6)],
        };
        assert_eq!(read_int32_array(&shifted), Some(vec![5, 6]));
        // A single unreadable element, unreadable bounds, empty and oversized arrays: None.
        assert_eq!(
            read_int32_array(&Arr {
                lo: Some(0),
                data: vec![Some(1), None]
            }),
            None
        );
        assert_eq!(
            read_int32_array(&Arr {
                lo: None,
                data: vec![Some(1)]
            }),
            None
        );
        assert_eq!(
            read_int32_array(&Arr {
                lo: Some(0),
                data: vec![]
            }),
            None
        );
        let big = Arr {
            lo: Some(0),
            data: vec![Some(1); MAX_RUNTIME_ID_LEN + 1],
        };
        assert_eq!(read_int32_array(&big), None);
        // Extreme bounds do not overflow.
        struct Wide;
        impl Int32Array for Wide {
            fn lower_bound(&self) -> Option<i32> {
                Some(i32::MIN)
            }
            fn upper_bound(&self) -> Option<i32> {
                Some(i32::MAX)
            }
            fn get(&self, _: i32) -> Option<i32> {
                Some(0)
            }
        }
        assert_eq!(read_int32_array(&Wide), None);
    }

    #[test]
    fn integrity_rids_map_and_order() {
        use IntegrityLevel::*;
        for (rid, lvl) in [
            (0, Untrusted),
            (0x1000, Low),
            (0x1FFF, Low),
            (0x2000, Medium),
            (0x20FF, Medium),
            (0x2100, MediumPlus),
            (0x3000, High),
            (0x4000, System),
            (0x5000, Protected),
            (0xFFFF_FFFF, Protected),
        ] {
            assert_eq!(IntegrityLevel::from_rid(rid), lvl, "{rid:#x}");
        }
        assert!(Untrusted < Low && Low < Medium && Medium < MediumPlus);
        assert!(MediumPlus < High && High < System && System < Protected);
        // The firewall refuses `integrity > own_integrity`.
        assert!(High.rank() > Medium.rank());
        assert!(Medium.rank() <= Medium.rank());
        assert!(Low.rank() < Medium.rank());
    }

    #[test]
    fn exe_paths_normalise_to_app_ids() {
        assert_eq!(
            app_id_from_exe_path(r"C:\Windows\System32\Notepad.EXE").as_deref(),
            Some("notepad.exe")
        );
        assert_eq!(
            app_id_from_exe_path("/x/y/Chrome.exe").as_deref(),
            Some("chrome.exe")
        );
        assert_eq!(
            app_id_from_exe_path("calc.exe").as_deref(),
            Some("calc.exe")
        );
        for bad in ["", r"C:\dir\", "C:\\dir\\ ", "C:\\a\\b\u{202E}.exe"] {
            assert_eq!(app_id_from_exe_path(bad), None, "{bad:?}");
        }
        // Same normalisation as the text source's `exe_name`: lowercase basename.
        assert_eq!(
            app_id_from_exe_path(r"D:\Apps\My App.exe").as_deref(),
            Some("my app.exe")
        );
    }

    #[test]
    fn process_identity_needs_pid_and_creation_time() {
        assert_eq!(filetime_ticks(1, 2), (2u64 << 32) | 1);
        assert!(same_process((10, 5), (10, 5)));
        assert!(!same_process((10, 5), (11, 5)), "pid differs");
        assert!(!same_process((10, 5), (10, 6)), "pid recycled");
        assert!(
            !same_process((10, 0), (10, 0)),
            "unreadable time never matches"
        );
        assert!(!same_process((0, 5), (0, 5)));
    }

    #[test]
    fn visible_window_filter() {
        assert!(include_in_apps(true, false, false, 10, 1));
        assert!(!include_in_apps(false, false, false, 10, 1));
        assert!(!include_in_apps(true, true, false, 10, 1));
        assert!(!include_in_apps(true, false, true, 10, 1));
        assert!(!include_in_apps(true, false, false, 1, 1), "our own pid");
        assert!(!include_in_apps(true, false, false, 0, 1));
    }

    #[test]
    fn desktop_names() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert!(is_user_desktop_name(&w("Default")));
        assert!(is_user_desktop_name(&w("default")));
        let mut padded = w("Default");
        padded.extend([0, 0, 0, 65]);
        assert!(is_user_desktop_name(&padded), "stops at the NUL");
        for bad in [
            "Winlogon",
            "Disconnect",
            "Screen-saver",
            "Default2",
            "Defaul",
            "",
        ] {
            assert!(!is_user_desktop_name(&w(bad)), "{bad}");
        }
        assert!(!is_user_desktop_name(&[
            0x0044, 0x0165, 0x0066, 0x0061, 0x0075, 0x006C, 0x0074
        ]));
        assert!(!is_user_desktop_name(&[]));
    }

    #[test]
    fn rects_convert_and_compare_exactly() {
        assert_eq!(
            rect_from_edges(10, 20, 110, 70),
            Some(Rect::new(10, 20, 100, 50))
        );
        assert_eq!(
            rect_from_edges(-5, -5, 5, 5),
            Some(Rect::new(-5, -5, 10, 10))
        );
        assert_eq!(rect_from_edges(10, 20, 9, 70), None);
        assert_eq!(rect_from_edges(10, 20, 110, 19), None);
        assert_eq!(
            rect_from_edges(i32::MIN, 0, i32::MAX, 1).map(|r| r.w),
            Some(u32::MAX)
        );
        let a = Rect::new(1, 2, 3, 4);
        assert!(rects_equal(&a, &Rect::new(1, 2, 3, 4)));
        assert!(!rects_equal(&a, &Rect::new(1, 2, 3, 5)));
        assert!(!rects_equal(&a, &Rect::new(2, 2, 3, 4)));
    }

    /// Chain 0 <- 1 <- 2 <- ... (parent of n is n+1) ending at `top`.
    fn chain(top: u32) -> impl FnMut(&u32) -> Result<Option<u32>, ()> {
        move |n| Ok((*n < top).then_some(*n + 1))
    }

    #[test]
    fn ancestor_walk_is_bounded() {
        let is = |t: u32| move |n: &u32| Ok::<bool, ()>(*n == t);
        assert_eq!(
            find_ancestor(&0, 64, chain(10), is(3)),
            Ok(AncestorOutcome::Found)
        );
        // The start itself is not an ancestor.
        assert_eq!(
            find_ancestor(&3, 64, chain(10), is(3)),
            Ok(AncestorOutcome::NotFound)
        );
        // Immediate parent.
        assert_eq!(
            find_ancestor(&2, 1, chain(10), is(3)),
            Ok(AncestorOutcome::Found)
        );
        // Root reached without meeting the target.
        assert_eq!(
            find_ancestor(&0, 64, chain(10), is(99)),
            Ok(AncestorOutcome::NotFound)
        );
        assert_eq!(
            find_ancestor(&10, 64, chain(10), is(99)),
            Ok(AncestorOutcome::NotFound)
        );
        // Exactly at the bound is found; one beyond is BoundExceeded (and never "found").
        assert_eq!(
            find_ancestor(&0, 5, chain(100), is(5)),
            Ok(AncestorOutcome::Found)
        );
        assert_eq!(
            find_ancestor(&0, 5, chain(100), is(6)),
            Ok(AncestorOutcome::BoundExceeded)
        );
        // A cycle cannot hang the walk.
        let cyc = |n: &u32| Ok::<Option<u32>, ()>(Some((*n + 1) % 3));
        assert_eq!(
            find_ancestor(&0, 64, cyc, is(99)),
            Ok(AncestorOutcome::BoundExceeded)
        );
        // Errors abort.
        let err = |_: &u32| Err::<Option<u32>, &str>("boom");
        assert_eq!(find_ancestor(&0, 8, err, |_| Ok(false)), Err("boom"));
    }

    #[test]
    fn node_identity_hashes_the_automation_id_and_rejects_bad_ids() {
        let i = node_identity(&[42, 1], 50000, "SubmitButton").unwrap();
        assert_eq!(i.runtime_id, vec![42, 1]);
        assert_eq!(i.control_type, 50000);
        assert_eq!(i.automation_id_hash, hash_automation_id("SubmitButton"));
        assert_ne!(i.automation_id_hash, hash_automation_id("submitbutton"));
        assert_eq!(i.label_hash, [0; 32], "the walker fills the label hash");
        assert!(node_identity(&[], 50000, "x").is_none());
        assert!(node_identity(&[1], -1, "x").is_none());
        // No automation id hashes like the empty string, same as the fake and the resolver.
        assert_eq!(
            node_identity(&[1], 50000, "").unwrap().automation_id_hash,
            hash_automation_id("")
        );
    }

    #[derive(Default)]
    struct Rec {
        log: Vec<&'static str>,
        password: bool,
        ct: i32,
        name: &'static str,
    }
    impl ElementReads for Rec {
        fn is_password(&mut self) -> Result<bool, FirewallError> {
            self.log.push("is_password");
            Ok(self.password)
        }
        fn control_type(&mut self) -> Result<i32, FirewallError> {
            self.log.push("control_type");
            Ok(self.ct)
        }
        fn automation_id(&mut self) -> Result<String, FirewallError> {
            self.log.push("automation_id");
            Ok("aid".into())
        }
        fn name(&mut self) -> Result<String, FirewallError> {
            self.log.push("name");
            Ok(self.name.into())
        }
        fn rect(&mut self) -> Result<Rect, FirewallError> {
            self.log.push("rect");
            Ok(Rect::new(1, 2, 3, 4))
        }
    }

    #[test]
    fn resolver_reads_is_password_first_and_nothing_else_for_secure_elements() {
        let mut e = Rec {
            password: true,
            ct: 50004,
            name: "Password",
            ..Default::default()
        };
        assert!(matches!(read_resolved(&mut e), Ok(ResolvedTarget::Secure)));
        assert_eq!(e.log, vec!["is_password"], "no name, id, type or rect read");

        let mut e = Rec {
            ct: 50000,
            name: "  Save ",
            ..Default::default()
        };
        let Ok(ResolvedTarget::Found(f)) = read_resolved(&mut e) else {
            panic!("found")
        };
        assert_eq!(e.log[0], "is_password");
        assert_eq!(f.control_type, 50000);
        assert_eq!(
            f.label_hash,
            hash_label(Some("Save")),
            "same hash as the walker's"
        );
        assert_eq!(f.automation_id_hash, hash_automation_id("aid"));
        assert_eq!(f.rect, Rect::new(1, 2, 3, 4));
        // Negative control types are errors.
        let mut e = Rec {
            ct: -1,
            ..Default::default()
        };
        assert!(read_resolved(&mut e).is_err());
    }

    struct Foc {
        log: Vec<&'static str>,
        password: bool,
        ct: i32,
        name: &'static str,
    }
    impl FocusedReads for Foc {
        fn is_password(&mut self) -> Result<bool, FirewallError> {
            self.log.push("is_password");
            Ok(self.password)
        }
        fn control_type(&mut self) -> Result<i32, FirewallError> {
            self.log.push("control_type");
            Ok(self.ct)
        }
        fn name(&mut self) -> Result<String, FirewallError> {
            self.log.push("name");
            Ok(self.name.into())
        }
    }

    #[test]
    fn focused_element_classification() {
        let mut f = Foc {
            log: vec![],
            password: true,
            ct: 50004,
            name: "x",
        };
        assert_eq!(classify_focused(&mut f), Ok(Some(FocusState::Secure)));
        assert_eq!(f.log, vec!["is_password"]);

        // Edit with a secret-ish name that is not IsPassword.
        let mut f = Foc {
            log: vec![],
            password: false,
            ct: 50004,
            name: "Enter OTP",
        };
        assert_eq!(classify_focused(&mut f), Ok(Some(FocusState::Sensitive)));
        assert_eq!(f.log, vec!["is_password", "control_type", "name"]);
        let mut f = Foc {
            log: vec![],
            password: false,
            ct: 50004,
            name: "Pass\u{200B}word",
        };
        assert_eq!(classify_focused(&mut f), Ok(Some(FocusState::Sensitive)));
        // Ordinary edit.
        let mut f = Foc {
            log: vec![],
            password: false,
            ct: 50004,
            name: "Search",
        };
        assert_eq!(classify_focused(&mut f), Ok(None));
        // A Document / Pane / Group / Button: the name is NOT even read (titles, D-003).
        for ct in [50030, 50033, 50026, 50000] {
            let mut f = Foc {
                log: vec![],
                password: false,
                ct,
                name: "My OTP token page",
            };
            assert_eq!(classify_focused(&mut f), Ok(None), "{ct}");
            assert_eq!(f.log, vec!["is_password", "control_type"], "{ct}");
        }
        assert!(focus_label_check_needed(50004) && focus_label_check_needed(50003));
        assert!(!focus_label_check_needed(50030));
    }

    // ---- a Windows-cursor-shaped fake: identity through the shared walker ----

    use super::super::walk::{walk_tree_identified, Limits, TreeCursor};
    use crate::NodeRole;
    use std::collections::HashMap;
    use std::time::Instant;

    struct El {
        password: bool,
        role: NodeRole,
        name: &'static str,
        automation_id: &'static str,
        rid: Vec<i32>,
        kids: Vec<usize>,
    }

    /// Mirrors `UiaCursor`: raw reads are private to the cursor, `identity` goes through
    /// `node_identity` (hashing the automation id) and REFUSES to run unless `is_secure` returned
    /// false for that node earlier.
    struct WinShaped {
        els: Vec<El>,
        secure_answers: HashMap<usize, bool>,
        identity_calls: Vec<usize>,
    }

    impl TreeCursor for WinShaped {
        type Node = usize;
        fn is_secure(&mut self, n: &usize) -> Result<bool, FirewallError> {
            let p = self.els[*n].password;
            self.secure_answers.insert(*n, p);
            Ok(p)
        }
        fn rect(&mut self, _: &usize) -> Result<Option<Rect>, FirewallError> {
            Ok(Some(Rect::new(0, 0, 10, 10)))
        }
        fn role(&mut self, n: &usize) -> Result<NodeRole, FirewallError> {
            Ok(self.els[*n].role)
        }
        fn is_editable(&mut self, _: &usize) -> Result<bool, FirewallError> {
            Ok(false)
        }
        fn has_value(&mut self, _: &usize) -> Result<bool, FirewallError> {
            Ok(false)
        }
        fn label(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
            Ok(Some(self.els[*n].name.to_string()))
        }
        fn text_value(&mut self, n: &usize) -> Result<Option<String>, FirewallError> {
            self.label(n)
        }
        fn children(&mut self, n: &usize) -> Result<Vec<usize>, FirewallError> {
            Ok(self.els[*n].kids.clone())
        }
        fn identity(&mut self, n: &usize) -> Result<NodeIdentity, FirewallError> {
            // Identity only after is_secure answered false for exactly this node.
            if self.secure_answers.get(n) != Some(&false) {
                return Err(FirewallError::Detection);
            }
            self.identity_calls.push(*n);
            let e = &self.els[*n];
            node_identity(&e.rid, 50000, e.automation_id).ok_or(FirewallError::Detection)
        }
    }

    #[test]
    fn windows_shaped_cursor_gets_identity_only_after_is_secure_and_only_for_emitted_nodes() {
        let el = |password, role, name, aid, rid: &[i32], kids: &[usize]| El {
            password,
            role,
            name,
            automation_id: aid,
            rid: rid.to_vec(),
            kids: kids.to_vec(),
        };
        let mut c = WinShaped {
            els: vec![
                el(false, NodeRole::Window, "Title", "", &[42], &[1, 2, 3]),
                el(false, NodeRole::Button, "Save", "btnSave", &[42, 1], &[]),
                el(true, NodeRole::TextField, "Password", "pw", &[42, 2], &[4]),
                el(false, NodeRole::Group, "Page title", "grp", &[42, 3], &[]),
                el(false, NodeRole::Button, "below secure", "x", &[42, 4], &[]),
            ],
            secure_answers: HashMap::new(),
            identity_calls: vec![],
        };
        let (tree, ids) = walk_tree_identified(
            &mut c,
            0,
            "app.exe".into(),
            &Limits::standard(Instant::now()),
        )
        .unwrap();
        // Window root, secure field, unlabeled top-level group: no identity.
        assert_eq!(c.identity_calls, vec![1]);
        assert_eq!(tree.nodes.len(), ids.len());
        let save = ids[1].as_ref().expect("button has identity");
        assert_eq!(save.runtime_id, vec![42, 1]);
        assert_eq!(save.automation_id_hash, hash_automation_id("btnSave"));
        assert_eq!(
            save.label_hash,
            hash_label(Some("Save")),
            "walker hashes the label it read"
        );
        assert!(ids[0].is_none() && ids[2].is_none() && ids[3].is_none());
        assert!(tree.nodes[2].secure && tree.nodes[2].label.is_none());
    }
}
