//! Pure planning for the Windows actuator (S2-14d2): everything that can be decided without the OS
//! is decided here and unit-tested on Linux. `platform/windows.rs` converts these plain data
//! structs to FFI structs at the boundary and does nothing else.
//!
//! * `KeyCombo` -> key table (virtual key, scan code, extended flag); chords press modifiers first
//!   and release them in reverse order.
//! * UTF-16 chunking of typed text (surrogate pairs always stay in one batch) and the event list
//!   (down + up per unit).
//! * Pressed-key bookkeeping (what `release_all` must let go of).
//! * Launch path validation / construction, scroll plans and error mapping.
//!
//! Typed text appears ONLY as UTF-16 units inside an event list that goes straight to the OS; the
//! `Debug` impls here never print a unit or a code point.
// Used by the Windows actuator and tests only.
#![cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]

use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use action::{KeyCombo, ScrollAmount, ScrollDirection};

use crate::actuator::MAX_CHUNK_CHARS;
use crate::error::ActError;

/// Upper bound of UTF-16 units in one batch: 16 characters, each at most 2 units, doubled with
/// headroom. A bigger chunk is refused (the driver never sends one).
pub(crate) const MAX_UNITS: usize = 64;
// 16 characters are at most 32 UTF-16 units: the cap leaves headroom and is never the binding one.
const _: () = assert!(MAX_CHUNK_CHARS * 2 <= MAX_UNITS);

// ---------- keys ----------

/// One physical key: virtual-key code, set-1 scan code and whether it is an "extended" key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Key {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
}

const fn key(vk: u16, scan: u16, extended: bool) -> Key {
    Key { vk, scan, extended }
}

pub(crate) const SHIFT: Key = key(0x10, 0x2A, false);
pub(crate) const CTRL: Key = key(0x11, 0x1D, false);
const ENTER: Key = key(0x0D, 0x1C, false);
const ESCAPE: Key = key(0x1B, 0x01, false);
const TAB: Key = key(0x09, 0x0F, false);
const UP: Key = key(0x26, 0x48, true);
const DOWN: Key = key(0x28, 0x50, true);
const LEFT: Key = key(0x25, 0x4B, true);
const RIGHT: Key = key(0x27, 0x4D, true);
const HOME: Key = key(0x24, 0x47, true);
const END: Key = key(0x23, 0x4F, true);
const PAGE_UP: Key = key(0x21, 0x49, true);
const PAGE_DOWN: Key = key(0x22, 0x51, true);
const SPACE: Key = key(0x20, 0x39, false);
const BACKSPACE: Key = key(0x08, 0x0E, false);
const DELETE: Key = key(0x2E, 0x53, true);
const KEY_A: Key = key(0x41, 0x1E, false);
const KEY_C: Key = key(0x43, 0x2E, false);
const KEY_F: Key = key(0x46, 0x21, false);
const KEY_Z: Key = key(0x5A, 0x2C, false);

/// The keys of a combo in press order: modifiers first, the main key last.
pub(crate) fn chord(k: KeyCombo) -> Vec<Key> {
    match k {
        KeyCombo::Enter => vec![ENTER],
        KeyCombo::Escape => vec![ESCAPE],
        KeyCombo::Tab => vec![TAB],
        KeyCombo::ShiftTab => vec![SHIFT, TAB],
        KeyCombo::Up => vec![UP],
        KeyCombo::Down => vec![DOWN],
        KeyCombo::Left => vec![LEFT],
        KeyCombo::Right => vec![RIGHT],
        KeyCombo::Home => vec![HOME],
        KeyCombo::End => vec![END],
        KeyCombo::PageUp => vec![PAGE_UP],
        KeyCombo::PageDown => vec![PAGE_DOWN],
        KeyCombo::Space => vec![SPACE],
        KeyCombo::Backspace => vec![BACKSPACE],
        KeyCombo::Delete => vec![DELETE],
        KeyCombo::SelectAll => vec![CTRL, KEY_A],
        KeyCombo::Undo => vec![CTRL, KEY_Z],
        KeyCombo::Find => vec![CTRL, KEY_F],
    }
}

/// `send_copy`: Ctrl+C and nothing else.
pub(crate) fn copy_chord() -> Vec<Key> {
    vec![CTRL, KEY_C]
}

// ---------- events ----------

/// Something that can be held down: a physical key or one UTF-16 unit typed as a Unicode event.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    Unicode(u16),
    Key(Key),
}

impl fmt::Debug for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the unit (typed text) in a Debug string.
        match self {
            Held::Unicode(_) => write!(f, "Held::Unicode"),
            Held::Key(k) => write!(f, "Held::Key(vk {:#04x})", k.vk),
        }
    }
}

/// One keyboard event as plain data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct InputEvent {
    pub held: Held,
    pub up: bool,
}

/// All keys down in order, then all up in reverse order.
pub(crate) fn chord_events(keys: &[Key]) -> Vec<InputEvent> {
    let mut v = Vec::with_capacity(keys.len() * 2);
    for k in keys {
        v.push(InputEvent {
            held: Held::Key(*k),
            up: false,
        });
    }
    for k in keys.iter().rev() {
        v.push(InputEvent {
            held: Held::Key(*k),
            up: true,
        });
    }
    v
}

/// Why a typed chunk is refused. Fixed tokens only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlanError {
    Empty,
    TooManyChars,
    TooManyUnits,
    Control,
}

impl PlanError {
    pub(crate) fn token(self) -> &'static str {
        match self {
            PlanError::Empty => "empty chunk",
            PlanError::TooManyChars => "chunk too long",
            PlanError::TooManyUnits => "chunk too large",
            PlanError::Control => "control character",
        }
    }
}

impl From<PlanError> for ActError {
    fn from(e: PlanError) -> Self {
        ActError::Platform(e.token())
    }
}

/// UTF-16 units of a chunk: at most [`MAX_CHUNK_CHARS`] characters and [`MAX_UNITS`] units, no
/// control characters (Enter is `KeyCombo::Enter`; a typed newline or tab never gets through).
pub(crate) fn utf16_units(chunk: &str) -> Result<Vec<u16>, PlanError> {
    let mut chars = 0usize;
    for c in chunk.chars() {
        if c.is_control() || c == '\0' {
            return Err(PlanError::Control);
        }
        chars += 1;
        if chars > MAX_CHUNK_CHARS {
            return Err(PlanError::TooManyChars);
        }
    }
    if chars == 0 {
        return Err(PlanError::Empty);
    }
    let units: Vec<u16> = chunk.encode_utf16().collect();
    if units.len() > MAX_UNITS {
        return Err(PlanError::TooManyUnits);
    }
    Ok(units)
}

/// The events of one chunk: down + up per UTF-16 unit, in order, all in ONE batch so a surrogate
/// pair can never be split between two calls.
pub(crate) fn char_events(chunk: &str) -> Result<Vec<InputEvent>, PlanError> {
    let units = utf16_units(chunk)?;
    let mut v = Vec::with_capacity(units.len() * 2);
    for u in units {
        v.push(InputEvent {
            held: Held::Unicode(u),
            up: false,
        });
        v.push(InputEvent {
            held: Held::Unicode(u),
            up: true,
        });
    }
    Ok(v)
}

/// Release events (key-up, reverse order of `held`).
pub(crate) fn release_events(held: &[Held]) -> Vec<InputEvent> {
    held.iter()
        .rev()
        .map(|h| InputEvent { held: *h, up: true })
        .collect()
}

/// What a batch of `events` of which `delivered` were accepted by the OS leaves held down.
pub(crate) fn still_down_after(events: &[InputEvent], delivered: usize) -> Vec<Held> {
    let mut down: Vec<Held> = Vec::new();
    for e in events.iter().take(delivered) {
        if e.up {
            down.retain(|h| *h != e.held);
        } else if !down.contains(&e.held) {
            down.push(e.held);
        }
    }
    down
}

// ---------- pressed-key bookkeeping ----------

/// Everything this actuator pressed and did not yet release, in press order.
#[derive(Default, Debug)]
pub(crate) struct PressedSet {
    held: Vec<Held>,
}

impl PressedSet {
    /// Before a batch goes out: every key-down in it counts as pressed (so a release from another
    /// thread, while the call is in flight, covers it).
    pub(crate) fn begin(&mut self, events: &[InputEvent]) {
        for e in events {
            if !e.up && !self.held.contains(&e.held) {
                self.held.push(e.held);
            }
        }
    }

    /// After a batch returned: `delivered` events were accepted. Replaces this batch's
    /// provisional entries with what is really still down. Returns how many entries remain held.
    pub(crate) fn settle(&mut self, events: &[InputEvent], delivered: usize) -> usize {
        self.held
            .retain(|h| !events.iter().any(|e| &e.held == h && !e.up));
        for h in still_down_after(events, delivered) {
            if !self.held.contains(&h) {
                self.held.push(h);
            }
        }
        self.held.len()
    }

    /// Empties the set and returns what was held (press order). Idempotent: the second call is
    /// empty.
    pub(crate) fn take_all(&mut self) -> Vec<Held> {
        std::mem::take(&mut self.held)
    }

    /// Puts back what a failed release could not let go of.
    pub(crate) fn restore(&mut self, held: Vec<Held>) {
        for h in held {
            if !self.held.contains(&h) {
                self.held.push(h);
            }
        }
    }

    fn is_empty_now(&self) -> bool {
        self.held.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.held.len()
    }
}

// ---------- release / recovery (pure over an injected "send events" function) ----------

/// Fixed token: a release failed twice in one call; keys stay in the set and the next call drains.
pub(crate) const KEYS_MAY_BE_HELD: &str = "input failed; keys may be held";

fn lock_set(m: &Mutex<PressedSet>) -> MutexGuard<'_, PressedSet> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One release attempt: takes everything held, sends the key-ups through `submit` (`(accepted,
/// win32 error)` per batch) and puts back what was not let go of. Returns `true` when nothing is
/// held afterwards. Idempotent; the lock is never held across `submit`, so a kill-path call can
/// run while another thread's batch is in flight.
pub(crate) fn release_all_with(
    pressed: &Mutex<PressedSet>,
    submit: &mut dyn FnMut(&[InputEvent]) -> (usize, u32),
) -> bool {
    let held = lock_set(pressed).take_all();
    if held.is_empty() {
        return true;
    }
    let events = release_events(&held);
    let (n, _) = submit(&events);
    if n == events.len() {
        true
    } else {
        // Not let go of: remembered, so the next release (or the next call's drain) tries again.
        lock_set(pressed).restore(held);
        false
    }
}

/// Start of every call: keys left over from an earlier double failure are released BEFORE any new
/// input. Safe because the driver runs one worker at a time.
pub(crate) fn drain_stale_with(
    pressed: &Mutex<PressedSet>,
    submit: &mut dyn FnMut(&[InputEvent]) -> (usize, u32),
) {
    if !lock_set(pressed).is_empty_now() {
        let _ = release_all_with(pressed, submit);
    }
}

/// Sends `events` as one batch through `submit` with the pressed-key bookkeeping around it.
/// Stale keys are drained first. After a partial batch the release is tried, and tried ONCE more
/// in this call; when it still fails the keys stay in the set and the error says they may be held.
pub(crate) fn send_batch_with(
    pressed: &Mutex<PressedSet>,
    events: &[InputEvent],
    submit: &mut dyn FnMut(&[InputEvent]) -> (usize, u32),
) -> Result<(), ActError> {
    if events.is_empty() {
        return Ok(());
    }
    drain_stale_with(pressed, submit);
    lock_set(pressed).begin(events);
    let (n, err) = submit(events);
    let n = n.min(events.len());
    let still_down = lock_set(pressed).settle(events, n);
    if n == events.len() {
        return Ok(());
    }
    if still_down > 0 && !release_all_with(pressed, submit) && !release_all_with(pressed, submit) {
        return Err(ActError::Platform(KEYS_MAY_BE_HELD));
    }
    Err(input_error(err))
}

// ---------- scrolling ----------

/// UIA `ScrollAmount` as plain data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScrollStep {
    NoAmount,
    SmallDecrement,
    LargeDecrement,
    SmallIncrement,
    LargeIncrement,
}

/// `(horizontal, vertical)` for one `ScrollPattern.Scroll` call.
pub(crate) fn scroll_plan(d: ScrollDirection, a: ScrollAmount) -> (ScrollStep, ScrollStep) {
    let (dec, inc) = match a {
        ScrollAmount::SmallStep => (ScrollStep::SmallDecrement, ScrollStep::SmallIncrement),
        ScrollAmount::LargePage => (ScrollStep::LargeDecrement, ScrollStep::LargeIncrement),
    };
    match d {
        ScrollDirection::Up => (ScrollStep::NoAmount, dec),
        ScrollDirection::Down => (ScrollStep::NoAmount, inc),
        ScrollDirection::Left => (dec, ScrollStep::NoAmount),
        ScrollDirection::Right => (inc, ScrollStep::NoAmount),
    }
}

// ---------- launching ----------

/// Longest accepted path in UTF-16 units (MAX_PATH minus the terminator).
const MAX_PATH_UNITS: usize = 259;

/// An absolute LOCAL drive path (`X:\dir\file`): not relative, not UNC, not a device / verbatim
/// path (`\\?\`, `\\.\`), no `/`, no `.` or `..` components, no empty components, no alternate
/// stream or wildcard characters, no quotes or control characters (the command line is built by
/// quoting). With `need_exe` the file name must end in `.exe`.
pub(crate) fn valid_local_path(p: &str, need_exe: bool) -> bool {
    let b = p.as_bytes();
    if p.encode_utf16().count() > MAX_PATH_UNITS || b.len() < 4 {
        return false;
    }
    if !(b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') {
        return false;
    }
    if p.chars().any(|c| {
        c.is_control() || matches!(c, '"' | '<' | '>' | '|' | '*' | '?' | '/' | '%' | '\u{0}')
    }) {
        return false;
    }
    let rest = &p[3..];
    let mut last = "";
    for comp in rest.split('\\') {
        if comp.is_empty()
            || comp == "."
            || comp == ".."
            || comp.contains(':')
            || comp.ends_with(' ')
            || comp.ends_with('.')
        {
            return false;
        }
        last = comp;
    }
    if need_exe {
        let l = last.to_ascii_lowercase();
        return l.len() > 4 && l.ends_with(".exe");
    }
    true
}

/// `<SystemRoot>\System32\notepad.exe`, built from the value of the `SystemRoot` environment
/// variable, which must itself be a valid absolute local directory. `None` otherwise: there is no
/// guess and no PATH search.
pub(crate) fn notepad_path(system_root: Option<&str>) -> Option<PathBuf> {
    let root = system_root?;
    if !valid_local_path(root, false) {
        return None;
    }
    let p = format!("{root}\\System32\\notepad.exe");
    valid_local_path(&p, true).then(|| PathBuf::from(p))
}

/// The command line `"<path>"` (no arguments). `None` for a path that fails
/// [`valid_local_path`] with `need_exe`.
pub(crate) fn quoted_command_line(path: &str) -> Option<String> {
    valid_local_path(path, true).then(|| format!("\"{path}\""))
}

// ---------- error mapping ----------

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_BAD_EXE_FORMAT: u32 = 193;
const ERROR_ELEVATION_REQUIRED: u32 = 740;

/// `CreateProcessW` failure -> fixed token. An elevation request is a refusal, never a prompt.
pub(crate) fn launch_error(code: u32) -> ActError {
    ActError::Platform(match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => "launch target missing",
        ERROR_ACCESS_DENIED => "launch access denied",
        ERROR_BAD_EXE_FORMAT => "launch target is not an executable",
        ERROR_ELEVATION_REQUIRED => "launch needs elevation",
        _ => "launch failed",
    })
}

/// The Win32 error code inside an `HRESULT_FROM_WIN32` value (`0x8007xxxx`); `0` for any other
/// facility (then the caller reports the generic token).
pub(crate) fn win32_from_hresult(hr: i32) -> u32 {
    let v = hr as u32;
    if v >> 16 == 0x8007 {
        v & 0xFFFF
    } else {
        0
    }
}

/// A top-level window may be brought to the front for `focus_app`: visible, not cloaked, not
/// minimised, unowned (a real top-level, not a dialog's helper), not ours, with a process.
pub(crate) fn window_qualifies(
    visible: bool,
    cloaked: bool,
    minimized: bool,
    owned: bool,
    pid: u32,
    own_pid: u32,
) -> bool {
    visible && !cloaked && !minimized && !owned && pid != 0 && pid != own_pid
}

/// `focus_app` may bring a window of a process to the front only when both integrity levels are
/// known and the candidate's is not higher than ours (an unknown one is skipped: fail closed).
pub(crate) fn integrity_allows(candidate: Option<u32>, own: Option<u32>) -> bool {
    matches!((candidate, own), (Some(c), Some(o)) if c <= o)
}

/// Keyboard-batch failure code -> fixed token (`GetLastError` after a return of 0).
pub(crate) fn input_error(code: u32) -> ActError {
    ActError::Platform(match code {
        ERROR_ACCESS_DENIED => "input blocked",
        _ => "input failed",
    })
}

const UIA_E_ELEMENTNOTENABLED: i32 = 0x8004_0200_u32 as i32;
const UIA_E_ELEMENTNOTAVAILABLE: i32 = 0x8004_0201_u32 as i32;
const UIA_E_NOCLICKABLEPOINT: i32 = 0x8004_0202_u32 as i32;
const UIA_E_NOTSUPPORTED: i32 = 0x8004_0204_u32 as i32;
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;

/// A failed UIA pattern call -> `ActError`. "Pattern missing" is `Unsupported` (nothing was
/// sent); everything else is `Platform` (effect unknown). Never an OS message.
pub(crate) fn uia_error(hr: i32) -> ActError {
    match hr {
        UIA_E_NOTSUPPORTED => ActError::Unsupported("pattern not supported"),
        UIA_E_ELEMENTNOTENABLED => ActError::Platform("element not enabled"),
        UIA_E_ELEMENTNOTAVAILABLE => ActError::Platform("element gone"),
        UIA_E_NOCLICKABLEPOINT => ActError::Platform("element not reachable"),
        E_ACCESSDENIED => ActError::Platform("access denied"),
        _ => ActError::Platform("uia call failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [KeyCombo; 18] = [
        KeyCombo::Enter,
        KeyCombo::Escape,
        KeyCombo::Tab,
        KeyCombo::ShiftTab,
        KeyCombo::Up,
        KeyCombo::Down,
        KeyCombo::Left,
        KeyCombo::Right,
        KeyCombo::Home,
        KeyCombo::End,
        KeyCombo::PageUp,
        KeyCombo::PageDown,
        KeyCombo::Space,
        KeyCombo::Backspace,
        KeyCombo::Delete,
        KeyCombo::SelectAll,
        KeyCombo::Undo,
        KeyCombo::Find,
    ];

    fn vks(k: KeyCombo) -> Vec<u16> {
        chord(k).iter().map(|k| k.vk).collect()
    }

    #[test]
    fn every_combo_maps_and_the_table_is_exact() {
        // Exhaustiveness guard: the array above lists every discriminant 1..=18 once.
        let mut ds: Vec<u8> = ALL.iter().map(|k| *k as u8).collect();
        ds.sort_unstable();
        assert_eq!(ds, (1..=18).collect::<Vec<u8>>());
        let expect: [(KeyCombo, &[u16]); 18] = [
            (KeyCombo::Enter, &[0x0D]),
            (KeyCombo::Escape, &[0x1B]),
            (KeyCombo::Tab, &[0x09]),
            (KeyCombo::ShiftTab, &[0x10, 0x09]),
            (KeyCombo::Up, &[0x26]),
            (KeyCombo::Down, &[0x28]),
            (KeyCombo::Left, &[0x25]),
            (KeyCombo::Right, &[0x27]),
            (KeyCombo::Home, &[0x24]),
            (KeyCombo::End, &[0x23]),
            (KeyCombo::PageUp, &[0x21]),
            (KeyCombo::PageDown, &[0x22]),
            (KeyCombo::Space, &[0x20]),
            (KeyCombo::Backspace, &[0x08]),
            (KeyCombo::Delete, &[0x2E]),
            (KeyCombo::SelectAll, &[0x11, 0x41]),
            (KeyCombo::Undo, &[0x11, 0x5A]),
            (KeyCombo::Find, &[0x11, 0x46]),
        ];
        for (k, want) in expect {
            assert_eq!(vks(k), want, "{k:?}");
        }
    }

    #[test]
    fn modifiers_come_first_and_only_ctrl_and_shift_are_used() {
        for k in ALL {
            let c = chord(k);
            assert!(!c.is_empty() && c.len() <= 2, "{k:?}");
            for m in &c[..c.len() - 1] {
                assert!(*m == CTRL || *m == SHIFT, "{k:?}");
            }
            // The main key is never a modifier, never a Win/Alt/F-key/V/X/W/S/P/R/L/T/N key.
            let main = c[c.len() - 1];
            assert!(main != CTRL && main != SHIFT);
            for banned in [
                0x5B, 0x5C, 0x12, 0x56, 0x58, 0x57, 0x53, 0x50, 0x52, 0x4C, 0x54, 0x4E,
            ] {
                assert_ne!(main.vk, banned, "{k:?}");
            }
            assert!(!(0x70..=0x87).contains(&main.vk), "{k:?} is an F-key");
        }
        assert_eq!(chord(KeyCombo::ShiftTab), vec![SHIFT, TAB]);
        assert_eq!(chord(KeyCombo::SelectAll), vec![CTRL, KEY_A]);
        assert_eq!(chord(KeyCombo::Undo), vec![CTRL, KEY_Z]);
        assert_eq!(chord(KeyCombo::Find), vec![CTRL, KEY_F]);
        assert_eq!(copy_chord(), vec![CTRL, KEY_C]);
    }

    #[test]
    fn scan_codes_and_extended_flags() {
        let ext = [
            KeyCombo::Up,
            KeyCombo::Down,
            KeyCombo::Left,
            KeyCombo::Right,
            KeyCombo::Home,
            KeyCombo::End,
            KeyCombo::PageUp,
            KeyCombo::PageDown,
            KeyCombo::Delete,
        ];
        for k in ALL {
            let c = chord(k);
            let main = c[c.len() - 1];
            assert_eq!(main.extended, ext.contains(&k), "{k:?}");
            assert_ne!(main.scan, 0, "{k:?}");
            for m in &c[..c.len() - 1] {
                assert!(!m.extended);
            }
        }
        // Backspace is NOT extended, Delete is.
        let (b, d) = (BACKSPACE, DELETE);
        assert!(!b.extended && d.extended);
        assert_eq!((ENTER.scan, TAB.scan, SPACE.scan), (0x1C, 0x0F, 0x39));
        assert_eq!((SHIFT.scan, CTRL.scan), (0x2A, 0x1D));
        assert_eq!(
            (UP.scan, DOWN.scan, LEFT.scan, RIGHT.scan),
            (0x48, 0x50, 0x4B, 0x4D)
        );
        // Every key of the table is distinct.
        let mut seen = Vec::new();
        for k in ALL {
            for key in chord(k) {
                if !seen.contains(&key) {
                    seen.push(key);
                }
            }
        }
        let mut vk: Vec<u16> = seen.iter().map(|k| k.vk).collect();
        vk.sort_unstable();
        vk.dedup();
        assert_eq!(vk.len(), seen.len());
    }

    #[test]
    fn chords_press_in_order_and_release_in_reverse() {
        let ev = chord_events(&chord(KeyCombo::ShiftTab));
        let shape: Vec<(u16, bool)> = ev
            .iter()
            .map(|e| match e.held {
                Held::Key(k) => (k.vk, e.up),
                Held::Unicode(_) => unreachable!(),
            })
            .collect();
        assert_eq!(
            shape,
            [(0x10, false), (0x09, false), (0x09, true), (0x10, true)]
        );
        // Every chord is balanced: each key goes down once and up once.
        for k in ALL {
            let ev = chord_events(&chord(k));
            assert_eq!(ev.iter().filter(|e| e.up).count(), ev.len() / 2);
            assert!(still_down_after(&ev, ev.len()).is_empty(), "{k:?}");
        }
    }

    #[test]
    fn chunking_ascii_bmp_and_surrogates() {
        let u = utf16_units("hello").unwrap();
        assert_eq!(u, "hello".encode_utf16().collect::<Vec<_>>());
        // BMP (non-ASCII): one unit per character.
        assert_eq!(utf16_units("h\u{e9}\u{4e2d}").unwrap().len(), 3);
        // A supplementary character is a surrogate pair, kept in ONE batch, high before low.
        let ev = char_events("\u{1F600}").unwrap();
        assert_eq!(ev.len(), 4);
        let units: Vec<(u16, bool)> = ev
            .iter()
            .map(|e| match e.held {
                Held::Unicode(u) => (u, e.up),
                Held::Key(_) => unreachable!(),
            })
            .collect();
        assert_eq!(
            units,
            [
                (0xD83D, false),
                (0xD83D, true),
                (0xDE00, false),
                (0xDE00, true)
            ]
        );
        // 16 emoji = 32 units = 64 events: allowed, in one batch.
        let s: String = "\u{1F600}".repeat(16);
        assert_eq!(utf16_units(&s).unwrap().len(), 32);
        assert_eq!(char_events(&s).unwrap().len(), 64);
        // 16 plain characters.
        assert_eq!(char_events(&"a".repeat(16)).unwrap().len(), 32);
    }

    #[test]
    fn chunk_limits_and_hostile_input() {
        assert_eq!(utf16_units(""), Err(PlanError::Empty));
        assert_eq!(utf16_units(&"a".repeat(17)), Err(PlanError::TooManyChars));
        assert_eq!(
            utf16_units(&"\u{1F600}".repeat(17)),
            Err(PlanError::TooManyChars)
        );
        for bad in ["a\nb", "\t", "x\u{0}", "\r", "\u{7f}", "\u{1b}"] {
            assert_eq!(utf16_units(bad), Err(PlanError::Control), "{bad:?}");
        }
        // The unit cap is the second line of defence (16 chars * 2 units = 32 <= 64 today).
        // Error mapping is a fixed token.
        assert_eq!(
            ActError::from(PlanError::TooManyChars),
            ActError::Platform("chunk too long")
        );
    }

    #[test]
    fn debug_never_prints_typed_units() {
        let ev = char_events("secret").unwrap();
        let s = format!("{ev:?}");
        assert!(
            !s.contains("115") && !s.contains("0x73") && !s.contains("secret"),
            "{s}"
        );
        assert!(format!("{:?}", Held::Unicode(0x41)).contains("Unicode"));
    }

    #[test]
    fn still_down_after_partial_delivery() {
        let ev = chord_events(&chord(KeyCombo::SelectAll)); // ctrl+, a+, a-, ctrl-
        assert!(still_down_after(&ev, 0).is_empty());
        assert_eq!(still_down_after(&ev, 1), vec![Held::Key(CTRL)]);
        assert_eq!(
            still_down_after(&ev, 2),
            vec![Held::Key(CTRL), Held::Key(KEY_A)]
        );
        assert_eq!(still_down_after(&ev, 3), vec![Held::Key(CTRL)]);
        assert!(still_down_after(&ev, 4).is_empty());
        // More delivered than events is clamped.
        assert!(still_down_after(&ev, 99).is_empty());
        // A Unicode down without its up is held.
        let t = char_events("ab").unwrap();
        assert_eq!(still_down_after(&t, 1), vec![Held::Unicode(0x61)]);
        assert_eq!(still_down_after(&t, 3), vec![Held::Unicode(0x62)]);
    }

    #[test]
    fn pressed_set_tracks_and_releases_idempotently() {
        let mut p = PressedSet::default();
        assert!(p.is_empty());
        let ev = chord_events(&chord(KeyCombo::Undo));
        p.begin(&ev);
        assert_eq!(
            p.len(),
            2,
            "both provisionally held while the call is in flight"
        );
        // A release from the kill path now lets go of both, last pressed first.
        let held = p.take_all();
        assert_eq!(held, vec![Held::Key(CTRL), Held::Key(KEY_Z)]);
        let rel = release_events(&held);
        assert_eq!(
            rel,
            vec![
                InputEvent {
                    held: Held::Key(KEY_Z),
                    up: true
                },
                InputEvent {
                    held: Held::Key(CTRL),
                    up: true
                }
            ]
        );
        assert!(rel.iter().all(|e| e.up));
        // Idempotent.
        assert!(p.take_all().is_empty());
        assert!(release_events(&p.take_all()).is_empty());
        // The call returned after the release: everything delivered, nothing left held.
        assert_eq!(p.settle(&ev, ev.len()), 0);
        assert!(p.is_empty());
    }

    #[test]
    fn pressed_set_settle_keeps_only_what_is_really_down() {
        let mut p = PressedSet::default();
        let ev = chord_events(&chord(KeyCombo::ShiftTab));
        p.begin(&ev);
        // Only the first two events made it: both keys are still down.
        assert_eq!(p.settle(&ev, 2), 2);
        assert_eq!(p.take_all(), vec![Held::Key(SHIFT), Held::Key(TAB)]);
        // A failed release is put back and retried.
        p.restore(vec![Held::Key(SHIFT)]);
        p.restore(vec![Held::Key(SHIFT)]);
        assert_eq!(p.len(), 1, "no duplicates");
        // Nothing delivered at all: nothing held.
        let mut q = PressedSet::default();
        q.begin(&ev);
        assert_eq!(q.settle(&ev, 0), 0);
        // Full success.
        let mut r = PressedSet::default();
        r.begin(&ev);
        assert_eq!(r.settle(&ev, 4), 0);
    }

    /// Scripted OS: `script` gives how many events each successive call accepts (`None` = all);
    /// every call is recorded.
    struct Os {
        script: Vec<Option<usize>>,
        calls: Vec<Vec<InputEvent>>,
    }

    impl Os {
        fn new(script: Vec<Option<usize>>) -> Self {
            Os {
                script,
                calls: Vec::new(),
            }
        }
        fn submit(&mut self, ev: &[InputEvent]) -> (usize, u32) {
            let i = self.calls.len();
            self.calls.push(ev.to_vec());
            let n = self.script.get(i).copied().flatten().unwrap_or(ev.len());
            (n, if n == ev.len() { 0 } else { 5 })
        }
    }

    fn ctrl_a() -> Vec<InputEvent> {
        chord_events(&chord(KeyCombo::SelectAll))
    }

    #[test]
    fn partial_batch_then_release_fails_once_retries_in_the_same_call_and_succeeds() {
        let set = Mutex::new(PressedSet::default());
        // batch: 2 of 4 accepted (ctrl, a down); release fails; release retry succeeds.
        let mut os = Os::new(vec![Some(2), Some(0), None]);
        let r = send_batch_with(&set, &ctrl_a(), &mut |e| os.submit(e));
        assert_eq!(r, Err(ActError::Platform("input blocked")));
        assert_eq!(os.calls.len(), 3, "batch, failed release, retried release");
        assert!(lock_set(&set).is_empty(), "nothing left held");
    }

    #[test]
    fn partial_batch_then_release_fails_twice_keeps_keys_and_says_so() {
        let set = Mutex::new(PressedSet::default());
        let mut os = Os::new(vec![Some(2), Some(0), Some(1)]);
        let r = send_batch_with(&set, &ctrl_a(), &mut |e| os.submit(e));
        assert_eq!(r, Err(ActError::Platform(KEYS_MAY_BE_HELD)));
        assert_eq!(os.calls.len(), 3);
        assert_eq!(lock_set(&set).len(), 2, "both keys remembered");
    }

    #[test]
    fn next_call_after_a_double_release_failure_drains_stale_keys_before_new_input() {
        let set = Mutex::new(PressedSet::default());
        let mut os = Os::new(vec![Some(2), Some(0), Some(0)]);
        assert!(send_batch_with(&set, &ctrl_a(), &mut |e| os.submit(e)).is_err());
        assert_eq!(lock_set(&set).len(), 2);
        // The OS works again: the drain (call 4) precedes the new batch (call 5).
        let next = chord_events(&chord(KeyCombo::Enter));
        assert_eq!(send_batch_with(&set, &next, &mut |e| os.submit(e)), Ok(()));
        assert_eq!(os.calls.len(), 5);
        assert!(os.calls[3].iter().all(|e| e.up), "drain is key-ups only");
        assert_eq!(os.calls[3].len(), 2);
        assert_eq!(os.calls[4], next);
        assert!(lock_set(&set).is_empty());
    }

    #[test]
    fn drain_stale_with_an_empty_set_sends_nothing_and_a_failed_drain_keeps_the_keys() {
        let set = Mutex::new(PressedSet::default());
        let mut os = Os::new(vec![]);
        drain_stale_with(&set, &mut |e| os.submit(e));
        assert!(os.calls.is_empty());
        lock_set(&set).restore(vec![Held::Key(CTRL)]);
        let mut bad = Os::new(vec![Some(0)]);
        drain_stale_with(&set, &mut |e| bad.submit(e));
        assert_eq!(
            lock_set(&set).len(),
            1,
            "still remembered for the next drain"
        );
    }

    #[test]
    fn kill_thread_release_all_during_an_in_flight_batch_is_idempotent() {
        let set = Mutex::new(PressedSet::default());
        let mut kill_os = Os::new(vec![]);
        let mut batch_os = Os::new(vec![]);
        let ev = ctrl_a();
        // While the batch is "in flight" the kill path releases twice.
        let mut kill_results = Vec::new();
        let r = send_batch_with(&set, &ev, &mut |e| {
            if e.iter().any(|x| !x.up) {
                kill_results.push(release_all_with(&set, &mut |k| kill_os.submit(k)));
                kill_results.push(release_all_with(&set, &mut |k| kill_os.submit(k)));
            }
            batch_os.submit(e)
        });
        assert_eq!(r, Ok(()));
        assert_eq!(kill_results, vec![true, true]);
        assert_eq!(kill_os.calls.len(), 1, "second release found nothing to do");
        assert!(kill_os.calls[0].iter().all(|e| e.up));
        assert!(lock_set(&set).is_empty());
    }

    #[test]
    fn scroll_plans() {
        use ScrollStep::*;
        assert_eq!(
            scroll_plan(ScrollDirection::Up, ScrollAmount::SmallStep),
            (NoAmount, SmallDecrement)
        );
        assert_eq!(
            scroll_plan(ScrollDirection::Down, ScrollAmount::LargePage),
            (NoAmount, LargeIncrement)
        );
        assert_eq!(
            scroll_plan(ScrollDirection::Left, ScrollAmount::LargePage),
            (LargeDecrement, NoAmount)
        );
        assert_eq!(
            scroll_plan(ScrollDirection::Right, ScrollAmount::SmallStep),
            (SmallIncrement, NoAmount)
        );
    }

    #[test]
    fn launch_paths() {
        let ok = |p: &str| valid_local_path(p, true);
        assert!(ok(r"C:\Windows\System32\notepad.exe"));
        assert!(ok(r"d:\Program Files\App\app.EXE"));
        for bad in [
            "notepad.exe",
            r"System32\notepad.exe",
            r"\Windows\notepad.exe",
            r"\\server\share\notepad.exe",
            r"\\?\C:\Windows\notepad.exe",
            r"\\.\C:\notepad.exe",
            r"C:notepad.exe",
            r"C:\Windows\..\Windows\notepad.exe",
            r"C:\Windows\.\notepad.exe",
            r"C:\Windows\\notepad.exe",
            "C:/Windows/notepad.exe",
            r"C:\Windows\notepad.exe:stream",
            r"C:\Windows\note*.exe",
            "C:\\Win\"dows\\notepad.exe",
            "C:\\Windows\\notepad.exe\0",
            r"C:\Windows\notepad.txt",
            r"C:\Windows\.exe",
            r"C:\Windows\notepad.exe.",
            r"C:\Windows\%SystemRoot%\notepad.exe",
            "",
            r"C:\",
        ] {
            assert!(!ok(bad), "{bad:?}");
        }
        let long = format!(r"C:\{}\a.exe", "x".repeat(300));
        assert!(!ok(&long));
        // Directories (no .exe needed).
        assert!(valid_local_path(r"C:\Windows", false));
        assert!(!valid_local_path(r"C:\Windows\", false));
        assert!(!valid_local_path(r"\\host\share", false));
    }

    #[test]
    fn notepad_resolves_only_under_a_valid_system_root() {
        assert_eq!(
            notepad_path(Some(r"C:\Windows")),
            Some(PathBuf::from(r"C:\Windows\System32\notepad.exe"))
        );
        assert_eq!(
            notepad_path(Some(r"D:\WINNT")),
            Some(PathBuf::from(r"D:\WINNT\System32\notepad.exe"))
        );
        for bad in [
            None,
            Some(""),
            Some("Windows"),
            Some(r"\\evil\share"),
            Some(r"\\?\C:\Windows"),
            Some(r"C:\Windows\..\Temp"),
            Some(r"C:\Windows\"),
        ] {
            assert!(notepad_path(bad).is_none(), "{bad:?}");
        }
        // The name is fixed: never a PATH search result.
        let p = notepad_path(Some(r"C:\Windows")).unwrap();
        assert!(p
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(r"\system32\notepad.exe"));
    }

    #[test]
    fn command_line_is_the_quoted_path_only() {
        assert_eq!(
            quoted_command_line(r"C:\Program Files\A\a.exe").as_deref(),
            Some("\"C:\\Program Files\\A\\a.exe\"")
        );
        assert!(quoted_command_line("a.exe").is_none());
        assert!(quoted_command_line("C:\\a\\b.exe\" /c calc").is_none());
    }

    #[test]
    fn hresult_win32_extraction_and_window_filter() {
        assert_eq!(win32_from_hresult(0x8007_02E4_u32 as i32), 740);
        assert_eq!(win32_from_hresult(0x8007_0002_u32 as i32), 2);
        assert_eq!(win32_from_hresult(0x8004_0200_u32 as i32), 0);
        assert_eq!(win32_from_hresult(0), 0);
        assert!(window_qualifies(true, false, false, false, 10, 99));
        for (v, c, m, o, pid) in [
            (false, false, false, false, 10),
            (true, true, false, false, 10),
            (true, false, true, false, 10),
            (true, false, false, true, 10),
            (true, false, false, false, 0),
            (true, false, false, false, 99),
        ] {
            assert!(
                !window_qualifies(v, c, m, o, pid, 99),
                "{v} {c} {m} {o} {pid}"
            );
        }
    }

    #[test]
    fn focus_skips_windows_of_higher_or_unknown_integrity() {
        const MEDIUM: u32 = 0x2000;
        const HIGH: u32 = 0x3000;
        assert!(integrity_allows(Some(MEDIUM), Some(MEDIUM)));
        assert!(integrity_allows(Some(0x1000), Some(MEDIUM)));
        assert!(!integrity_allows(Some(HIGH), Some(MEDIUM)));
        assert!(!integrity_allows(None, Some(MEDIUM)));
        assert!(!integrity_allows(Some(MEDIUM), None));
        assert!(!integrity_allows(None, None));
    }

    #[test]
    fn error_mapping_is_fixed_tokens() {
        assert_eq!(
            launch_error(740),
            ActError::Platform("launch needs elevation")
        );
        assert_eq!(launch_error(2), ActError::Platform("launch target missing"));
        assert_eq!(launch_error(3), ActError::Platform("launch target missing"));
        assert_eq!(launch_error(5), ActError::Platform("launch access denied"));
        assert_eq!(
            launch_error(193),
            ActError::Platform("launch target is not an executable")
        );
        assert_eq!(launch_error(12345), ActError::Platform("launch failed"));
        assert_eq!(input_error(5), ActError::Platform("input blocked"));
        assert_eq!(input_error(0), ActError::Platform("input failed"));
        assert_eq!(
            uia_error(0x8004_0204_u32 as i32),
            ActError::Unsupported("pattern not supported")
        );
        assert_eq!(
            uia_error(0x8004_0200_u32 as i32),
            ActError::Platform("element not enabled")
        );
        assert_eq!(
            uia_error(0x8004_0201_u32 as i32),
            ActError::Platform("element gone")
        );
        assert_eq!(uia_error(-1), ActError::Platform("uia call failed"));
    }
}
