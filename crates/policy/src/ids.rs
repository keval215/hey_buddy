//! Small identifier types of the gate.

use std::fmt;

use sha2::{Digest, Sha256};

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One agent task (one spoken request). Plain data, not a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// Id of one pending approval. Single use. Knowing it is not enough to decide: `decide` also
/// needs the digest that was shown. Plain data (it travels over IPC).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalId(u64);

impl ApprovalId {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ApprovalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApprovalId({:016x})", self.0)
    }
}

/// Digest of one bound action (see `action::action_digest`). Debug/Display print the first 8 hex
/// digits only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionDigest([u8; 32]);

impl ActionDigest {
    pub(crate) fn new(b: [u8; 32]) -> Self {
        Self(b)
    }
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

impl fmt::Debug for ActionDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ActionDigest({})", self.short())
    }
}

impl fmt::Display for ActionDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

/// Approval ids are derived from the session nonce and a counter: unguessable, no OS call.
pub(crate) fn approval_id(nonce: &[u8; 32], counter: u64) -> ApprovalId {
    let mut h = Sha256::new();
    h.update(b"hey_buddy/approval/v1");
    h.update(nonce);
    h.update(counter.to_le_bytes());
    let d = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    ApprovalId(u64::from_le_bytes(b))
}

/// Per-gate id derived from the session nonce (domain separated; reveals nothing usable).
pub(crate) fn gate_id_of(nonce: &[u8; 32]) -> u64 {
    let mut h = Sha256::new();
    h.update(b"hey_buddy/gate/v1");
    h.update(nonce);
    let d = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    u64::from_le_bytes(b)
}
