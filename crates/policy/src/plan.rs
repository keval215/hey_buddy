//! Wire schema `plan/v1` and core-side re-validation (ADR 0003 section 4).
//!
//! # Wire schema
//! `{"steps":[{"op":..., ...}], "cannot":null|"<code>"}` (`cannot` may be omitted = null).
//!
//! | op | fields |
//! |---|---|
//! | `observe`, `copy_selection` | none |
//! | `find_text`, `answer_from_screen` | `query` (1..=200 chars) |
//! | `scroll` | `direction` `up`/`down`, `amount` 1..=10 |
//! | `focus_window`, `open_app` | `app` (1..=64 chars; must be said in the request and resolve in the catalogue) |
//! | `click`, `submit` | `target` |
//! | `type` | `target`, `text` (1..=1000 chars, rejected if it has any hostile char, newline or tab) |
//! | `key_combo` | `keys`: 1..=4 tokens, see [`crate::UnknownChord`] |
//!
//! `target` = `{"role"?: <role name>, "label": 1..=120 chars, "nth"?: 1..=5}`. There is no tier,
//! approval, url, path, argument, command or free-form field; any other field is an error.
//!
//! # Validation order (first failure wins; every error is a fixed token)
//! Size <= 16 KiB; valid UTF-8; no NUL; nesting depth <= 5; JSON syntax and duplicate keys;
//! unknown fields at every level; `cannot`; 1..=12 steps; then per step in order: op, field
//! types, string rules, ranges, key tokens and `type.text` through `action::TypedText::new`;
//! finally provenance of app names, then catalogue resolution.
//!
//! Strings are never silently cleaned: any `firewall::is_hostile_char` character rejects the
//! plan. Labels/queries/app names are then sanitised for display and matching with the firewall
//! screen-text mapping (`<`,`>` -> `‹`,`›`, `"` -> `'`, whitespace collapsed). Matching uses the
//! sanitised label only.
//!
//! # Canonical bytes and digest
//! Little-endian; `lp(x)` = u64 length ‖ bytes. Canonical plan = `0x01` ‖ u16 step count ‖ steps.
//! Step = tag (observe 1, find_text 2, answer_from_screen 3, scroll 4, focus_window 5, open_app
//! 6, click 7, type 8, submit 9, key_combo 10, key_chord 11, copy_selection 12) ‖ fields:
//! query `lp`; scroll u8 direction (`action::ScrollDirection`) ‖ u8 amount; focus_window `lp(name)`
//! ‖ `lp(app id)`; open_app `lp(name)` ‖ `lp(launch id)` ‖ `lp(app id)`; target = u8 role (0 none,
//! Window 1 .. Other 20 in declaration order) ‖ `lp(label)` ‖ u8 nth (0 none); type = target ‖
//! `lp(text)` ‖ u8 provenance (FromRequest 1, ModelGenerated 2); key_combo u8 `action::KeyCombo`
//! discriminant; key_chord u8 modifiers (ctrl 1, alt 2, shift 4, win 8) ‖ `lp(key or empty)`.
//! These bytes are core's re-serialisation, never the model's.
//!
//! `plan_digest` = `sha256("hey_buddy/plan/v1"` (17 bytes, no length prefix) ‖ `lp(session_nonce)`
//! ‖ `u64 task` ‖ `lp(canonical plan)` ‖ `lp(canonical dry-run bindings, or empty)`).

use std::fmt;
use std::sync::Arc;

use action::{KeyCombo, ScrollDirection, TypedText};
use firewall::{is_hostile_char, NodeRole};
use sha2::{Digest, Sha256};

use crate::catalogue::{AppEntry, Catalogue};
use crate::chord::{Parsed, UnknownChord};
use crate::error::PlanError;
use crate::norm;
use crate::parse::{self, Obj, Val};

pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
pub const MAX_STEPS: usize = 12;
pub const MAX_LABEL_CHARS: usize = 120;
pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_APP_NAME_CHARS: usize = 64;
/// ADR 0003 says 2000; `action::TypedText` caps at 1000 and the stricter value wins.
pub const MAX_TYPE_TEXT_CHARS: usize = action::MAX_TYPED_TEXT_CHARS;
pub const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
const MAX_KEY_TOKEN_CHARS: usize = 16;
const MAX_CANNOT_CHARS: usize = 32;

/// Where `type.text` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A normalised, whole-word substring of the user's transcript.
    FromRequest,
    /// Anything else: the user must read the exact text; never batchable.
    ModelGenerated,
}

/// A plan target: what to act on, by role and visible label (matched against the sanitised label).
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    role: Option<NodeRole>,
    label: String,
    nth: Option<u8>,
}

impl Target {
    /// The role the engine named (a hint; classification uses the OBSERVED role).
    pub fn role(&self) -> Option<NodeRole> {
        self.role
    }
    /// Sanitised label, 1..=120 chars.
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn nth(&self) -> Option<u8> {
        self.nth
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Target(role {:?}, label len {}, nth {:?})",
            self.role,
            self.label.chars().count(),
            self.nth
        )
    }
}

/// One validated step. Cannot be built outside this crate except through [`validate`].
#[derive(Clone, PartialEq, Eq)]
pub enum PlanStep {
    Observe,
    FindText {
        query: String,
    },
    AnswerFromScreen {
        query: String,
    },
    Scroll {
        direction: ScrollDirection,
        amount: u8,
    },
    FocusWindow {
        /// Normalised name as said by the user.
        name: String,
        app: AppEntry,
    },
    OpenApp {
        name: String,
        app: AppEntry,
    },
    Click {
        target: Target,
    },
    Type {
        target: Target,
        text: TypedText,
        provenance: Provenance,
    },
    Submit {
        target: Target,
    },
    /// A chord from the closed `action::KeyCombo` set.
    KeyCombo(KeyCombo),
    /// A chord outside the closed set (Ctrl+V, Alt+F4, Win+..., Ctrl+Enter ...): representable so
    /// it can be shown and refused, never convertible to an `action::Action`.
    KeyChord(UnknownChord),
    CopySelection,
}

impl PlanStep {
    /// Wire op name (`key_combo` for both typed and unknown chords).
    pub fn op_name(&self) -> &'static str {
        match self {
            PlanStep::Observe => "observe",
            PlanStep::FindText { .. } => "find_text",
            PlanStep::AnswerFromScreen { .. } => "answer_from_screen",
            PlanStep::Scroll { .. } => "scroll",
            PlanStep::FocusWindow { .. } => "focus_window",
            PlanStep::OpenApp { .. } => "open_app",
            PlanStep::Click { .. } => "click",
            PlanStep::Type { .. } => "type",
            PlanStep::Submit { .. } => "submit",
            PlanStep::KeyCombo(_) | PlanStep::KeyChord(_) => "key_combo",
            PlanStep::CopySelection => "copy_selection",
        }
    }

    pub fn target(&self) -> Option<&Target> {
        match self {
            PlanStep::Click { target }
            | PlanStep::Type { target, .. }
            | PlanStep::Submit { target } => Some(target),
            _ => None,
        }
    }

    /// The text of a `type` step.
    pub fn text(&self) -> Option<&TypedText> {
        match self {
            PlanStep::Type { text, .. } => Some(text),
            _ => None,
        }
    }

    pub fn provenance(&self) -> Option<Provenance> {
        match self {
            PlanStep::Type { provenance, .. } => Some(*provenance),
            _ => None,
        }
    }
}

/// No content: op name only.
impl fmt::Debug for PlanStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlanStep({})", self.op_name())
    }
}

/// Digest of a plan bound to a session and task. Debug/Display print the first 8 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanDigest([u8; 32]);

impl PlanDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
    /// First 8 hex digits, for audit and UI.
    pub fn short(&self) -> String {
        hex(&self.0[..4])
    }
}

impl fmt::Debug for PlanDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlanDigest({})", self.short())
    }
}

impl fmt::Display for PlanDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A plan that passed [`validate`]. Sealed: private fields, `pub(crate)` constructor, no `Clone`
/// (share with [`ValidatedPlan::into_shared`]), no `Default`, no serde, no `From`.
pub struct ValidatedPlan {
    steps: Vec<PlanStep>,
    canonical: Vec<u8>,
}

impl ValidatedPlan {
    pub(crate) fn from_steps(steps: Vec<PlanStep>) -> Self {
        let canonical = encode(&steps);
        Self { steps, canonical }
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Never true: a validated plan has at least one step.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Core's own canonical serialisation (see the module docs).
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// Cheap sharing without `Clone`.
    pub fn into_shared(self) -> Arc<ValidatedPlan> {
        Arc::new(self)
    }

    /// `sha256("hey_buddy/plan/v1" ‖ lp(nonce) ‖ u64 task ‖ lp(canonical plan) ‖ lp(bindings))`.
    /// `canonical_bindings` is the canonical dry-run bindings (S2-11b), or empty.
    pub fn plan_digest(
        &self,
        session_nonce: &[u8; 32],
        task: u64,
        canonical_bindings: &[u8],
    ) -> PlanDigest {
        let mut buf = Vec::with_capacity(self.canonical.len() + canonical_bindings.len() + 96);
        buf.extend_from_slice(b"hey_buddy/plan/v1");
        lp(&mut buf, session_nonce);
        buf.extend_from_slice(&task.to_le_bytes());
        lp(&mut buf, &self.canonical);
        lp(&mut buf, canonical_bindings);
        PlanDigest(Sha256::digest(&buf).into())
    }
}

/// Digest8 and step count only.
impl fmt::Debug for ValidatedPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = Sha256::digest(&self.canonical);
        write!(
            f,
            "ValidatedPlan({}, {} steps)",
            hex(&d[..4]),
            self.steps.len()
        )
    }
}

fn lp(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}

fn role_code(r: Option<NodeRole>) -> u8 {
    match r {
        None => 0,
        Some(NodeRole::Window) => 1,
        Some(NodeRole::Button) => 2,
        Some(NodeRole::Link) => 3,
        Some(NodeRole::MenuItem) => 4,
        Some(NodeRole::Tab) => 5,
        Some(NodeRole::CheckBox) => 6,
        Some(NodeRole::RadioButton) => 7,
        Some(NodeRole::ComboBox) => 8,
        Some(NodeRole::TextField) => 9,
        Some(NodeRole::StaticText) => 10,
        Some(NodeRole::Heading) => 11,
        Some(NodeRole::Image) => 12,
        Some(NodeRole::List) => 13,
        Some(NodeRole::ListItem) => 14,
        Some(NodeRole::Table) => 15,
        Some(NodeRole::Toolbar) => 16,
        Some(NodeRole::Group) => 17,
        Some(NodeRole::WebArea) => 18,
        Some(NodeRole::Opaque) => 19,
        Some(NodeRole::Other) => 20,
    }
}

fn enc_target(out: &mut Vec<u8>, t: &Target) {
    out.push(role_code(t.role));
    lp(out, t.label.as_bytes());
    out.push(t.nth.unwrap_or(0));
}

fn encode(steps: &[PlanStep]) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&(steps.len() as u16).to_le_bytes());
    for s in steps {
        match s {
            PlanStep::Observe => out.push(1),
            PlanStep::FindText { query } => {
                out.push(2);
                lp(&mut out, query.as_bytes());
            }
            PlanStep::AnswerFromScreen { query } => {
                out.push(3);
                lp(&mut out, query.as_bytes());
            }
            PlanStep::Scroll { direction, amount } => {
                out.push(4);
                out.push(*direction as u8);
                out.push(*amount);
            }
            PlanStep::FocusWindow { name, app } => {
                out.push(5);
                lp(&mut out, name.as_bytes());
                lp(&mut out, app.app.as_str().as_bytes());
            }
            PlanStep::OpenApp { name, app } => {
                out.push(6);
                lp(&mut out, name.as_bytes());
                lp(&mut out, app.launch.as_str().as_bytes());
                lp(&mut out, app.app.as_str().as_bytes());
            }
            PlanStep::Click { target } => {
                out.push(7);
                enc_target(&mut out, target);
            }
            PlanStep::Type {
                target,
                text,
                provenance,
            } => {
                out.push(8);
                enc_target(&mut out, target);
                lp(&mut out, text.as_str().as_bytes());
                out.push(match provenance {
                    Provenance::FromRequest => 1,
                    Provenance::ModelGenerated => 2,
                });
            }
            PlanStep::Submit { target } => {
                out.push(9);
                enc_target(&mut out, target);
            }
            PlanStep::KeyCombo(k) => {
                out.push(10);
                out.push(*k as u8);
            }
            PlanStep::KeyChord(c) => {
                out.push(11);
                c.canonical(&mut out);
            }
            PlanStep::CopySelection => out.push(12),
        }
    }
    out
}

/// Limits for one validation. Values above the hard caps are clamped (limits only tighten).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanLimits {
    pub max_steps: usize,
    pub max_draft_bytes: usize,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_steps: MAX_STEPS,
            max_draft_bytes: MAX_DRAFT_BYTES,
        }
    }
}

/// Everything `validate` needs besides the draft.
pub struct PlanContext<'a> {
    /// The user's spoken request (provenance source). Never parsed.
    pub transcript: &'a str,
    pub catalogue: &'a dyn Catalogue,
    pub limits: PlanLimits,
}

impl<'a> PlanContext<'a> {
    pub fn new(transcript: &'a str, catalogue: &'a dyn Catalogue) -> Self {
        Self {
            transcript,
            catalogue,
            limits: PlanLimits::default(),
        }
    }
}

impl fmt::Debug for PlanContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlanContext(transcript len {})", self.transcript.len())
    }
}

/// Validates an untrusted draft. See the module docs for the order of checks.
pub fn validate(draft: &str, ctx: &PlanContext<'_>) -> Result<ValidatedPlan, PlanError> {
    validate_bytes(draft.as_bytes(), ctx)
}

/// As [`validate`] for raw bytes (invalid UTF-8 is rejected).
pub fn validate_bytes(draft: &[u8], ctx: &PlanContext<'_>) -> Result<ValidatedPlan, PlanError> {
    let max_bytes = ctx.limits.max_draft_bytes.min(MAX_DRAFT_BYTES);
    let max_steps = ctx.limits.max_steps.min(MAX_STEPS);
    if ctx.transcript.len() > MAX_TRANSCRIPT_BYTES {
        return Err(PlanError::TranscriptTooLarge);
    }
    let root = parse::parse_bytes(draft, max_bytes)?;
    let mut root = Obj::from(root)?;
    let steps_v = root.take("steps");
    let cannot_v = root.take("cannot");
    root.finish()?;
    let steps_v = steps_v.ok_or(PlanError::MissingField)?;

    let cannot = match cannot_v {
        None | Some(Val::Null) => false,
        Some(Val::Str(code)) => {
            let ok = !code.is_empty()
                && code.chars().count() <= MAX_CANNOT_CHARS
                && code
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'));
            if !ok {
                return Err(PlanError::BadCannot);
            }
            true
        }
        Some(_) => return Err(PlanError::BadCannot),
    };
    let items = parse::as_array(steps_v)?;
    if cannot {
        return Err(if items.is_empty() {
            PlanError::CannotPlan
        } else {
            PlanError::BadCannot
        });
    }
    if items.is_empty() || items.len() > max_steps {
        return Err(PlanError::StepCount);
    }

    let cx = Cx {
        transcript: norm::provenance(ctx.transcript),
        catalogue: ctx.catalogue,
    };
    let steps = items
        .into_iter()
        .map(|v| step(v, &cx))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ValidatedPlan::from_steps(steps))
}

struct Cx<'a> {
    /// Transcript in `norm::provenance` form.
    transcript: String,
    catalogue: &'a dyn Catalogue,
}

#[derive(Clone, Copy)]
enum Kind {
    Label,
    Other,
}

/// Reject hostile characters (never clean them), cap, then sanitise for display/matching.
fn clean(v: Val, max: usize, kind: Kind) -> Result<String, PlanError> {
    let raw = parse::as_string(v)?;
    let (empty, long, hostile) = match kind {
        Kind::Label => (
            PlanError::LabelEmpty,
            PlanError::LabelTooLong,
            PlanError::StringHostile,
        ),
        Kind::Other => (
            PlanError::StringEmpty,
            PlanError::StringTooLong,
            PlanError::StringHostile,
        ),
    };
    let mut n = 0usize;
    for c in raw.chars() {
        if is_hostile_char(c) {
            return Err(hostile);
        }
        n += 1;
        if n > max {
            return Err(long);
        }
    }
    let s = norm::sanitise(&raw);
    if s.is_empty() {
        return Err(empty);
    }
    Ok(s)
}

fn small_int(v: Val, lo: u64, hi: u64, err: PlanError) -> Result<u8, PlanError> {
    match v {
        Val::Uint(n) if (lo..=hi).contains(&n) => Ok(n as u8),
        Val::Uint(_) | Val::OtherNum => Err(err),
        _ => Err(PlanError::WrongType),
    }
}

const ROLES: &[(&str, NodeRole)] = &[
    ("window", NodeRole::Window),
    ("button", NodeRole::Button),
    ("link", NodeRole::Link),
    ("menu_item", NodeRole::MenuItem),
    ("menu item", NodeRole::MenuItem),
    ("tab", NodeRole::Tab),
    ("checkbox", NodeRole::CheckBox),
    ("radio_button", NodeRole::RadioButton),
    ("radio button", NodeRole::RadioButton),
    ("combo_box", NodeRole::ComboBox),
    ("combo box", NodeRole::ComboBox),
    ("text_field", NodeRole::TextField),
    ("text field", NodeRole::TextField),
    ("static_text", NodeRole::StaticText),
    ("text", NodeRole::StaticText),
    ("heading", NodeRole::Heading),
    ("image", NodeRole::Image),
    ("list", NodeRole::List),
    ("list_item", NodeRole::ListItem),
    ("list item", NodeRole::ListItem),
    ("table", NodeRole::Table),
    ("toolbar", NodeRole::Toolbar),
    ("group", NodeRole::Group),
    ("web_area", NodeRole::WebArea),
    ("web area", NodeRole::WebArea),
    ("opaque", NodeRole::Opaque),
    ("other", NodeRole::Other),
    ("element", NodeRole::Other),
];

fn target(v: Val) -> Result<Target, PlanError> {
    let mut o = Obj::from(v)?;
    let role = match o.take("role") {
        None => None,
        Some(r) => {
            let s = parse::as_string(r)?;
            let found = ROLES.iter().find(|(n, _)| *n == s).map(|(_, r)| *r);
            Some(found.ok_or(PlanError::BadRole)?)
        }
    };
    let label = clean(o.require("label")?, MAX_LABEL_CHARS, Kind::Label)?;
    let nth = match o.take("nth") {
        None => None,
        Some(n) => Some(small_int(n, 1, 5, PlanError::BadNth)?),
    };
    o.finish()?;
    Ok(Target { role, label, nth })
}

fn app(v: Val, cx: &Cx<'_>) -> Result<(String, AppEntry), PlanError> {
    let shown = clean(v, MAX_APP_NAME_CHARS, Kind::Other)?;
    let name = norm::provenance(&shown);
    if !norm::contains_bounded(&cx.transcript, &name) {
        return Err(PlanError::AppNotInRequest);
    }
    let entry = cx.catalogue.resolve(&name).ok_or(PlanError::UnknownApp)?;
    Ok((name, entry))
}

fn step(v: Val, cx: &Cx<'_>) -> Result<PlanStep, PlanError> {
    let mut o = Obj::from(v)?;
    let op = parse::as_string(o.require("op")?)?;
    let st = match op.as_str() {
        "observe" => PlanStep::Observe,
        "copy_selection" => PlanStep::CopySelection,
        "find_text" => PlanStep::FindText {
            query: clean(o.require("query")?, MAX_QUERY_CHARS, Kind::Other)?,
        },
        "answer_from_screen" => PlanStep::AnswerFromScreen {
            query: clean(o.require("query")?, MAX_QUERY_CHARS, Kind::Other)?,
        },
        "scroll" => {
            let direction = match parse::as_string(o.require("direction")?)?.as_str() {
                "up" => ScrollDirection::Up,
                "down" => ScrollDirection::Down,
                _ => return Err(PlanError::BadDirection),
            };
            let amount = small_int(o.require("amount")?, 1, 10, PlanError::BadAmount)?;
            PlanStep::Scroll { direction, amount }
        }
        "focus_window" => {
            let (name, app) = app(o.require("app")?, cx)?;
            PlanStep::FocusWindow { name, app }
        }
        "open_app" => {
            let (name, app) = app(o.require("app")?, cx)?;
            PlanStep::OpenApp { name, app }
        }
        "click" => PlanStep::Click {
            target: target(o.require("target")?)?,
        },
        "submit" => PlanStep::Submit {
            target: target(o.require("target")?)?,
        },
        "type" => {
            let target = target(o.require("target")?)?;
            let raw = parse::as_string(o.require("text")?)?;
            let text = TypedText::new(&raw).map_err(|e| match e {
                action::ActionError::TextEmpty => PlanError::TextEmpty,
                action::ActionError::TextTooLong => PlanError::TextTooLong,
                _ => PlanError::TextHostile,
            })?;
            let n = norm::provenance(text.as_str());
            let provenance = if norm::contains_bounded(&cx.transcript, &n) {
                Provenance::FromRequest
            } else {
                Provenance::ModelGenerated
            };
            PlanStep::Type {
                target,
                text,
                provenance,
            }
        }
        "key_combo" => key_step(parse::as_array(o.require("keys")?)?)?,
        _ => return Err(PlanError::UnknownOp),
    };
    o.finish()?;
    Ok(st)
}

fn key_step(keys: Vec<Val>) -> Result<PlanStep, PlanError> {
    let mut toks = Vec::with_capacity(keys.len());
    for k in keys {
        let s = parse::as_string(k)?;
        if s.chars().count() > MAX_KEY_TOKEN_CHARS {
            return Err(PlanError::BadKey);
        }
        toks.push(s.to_lowercase());
    }
    match UnknownChord::parse_tokens(&toks).ok_or(PlanError::BadKey)? {
        Parsed::Combo(k) => Ok(PlanStep::KeyCombo(k)),
        Parsed::Copy => Ok(PlanStep::CopySelection),
        Parsed::Chord(c) => Ok(PlanStep::KeyChord(c)),
    }
}
