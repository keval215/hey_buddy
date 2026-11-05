use std::fmt;
use std::sync::Arc;

use firewall::{AppId, Observation, ObservationFlags, ObservationId, RedactedScreenText};

/// What the agent engine may see of the screen: the firewall-redacted screen text (`#n` numbered
/// lines, `apps:` header), the visible app ids and the flags. Built only from an
/// `&firewall::Observation`; private fields; no `Default`.
///
/// It may be serialised by `core` for the engine process (it is content the firewall already
/// cleared), but there is NO `Deserialize`: nothing can conjure an `ObservedScreen`.
///
/// ```compile_fail,E0451
/// // Private fields: no struct literal from outside the crate.
/// fn forge() -> driver::ObservedScreen {
///     driver::ObservedScreen {
///         id: todo!(),
///         text: todo!(),
///         apps: todo!(),
///         flags: todo!(),
///     }
/// }
/// ```
/// ```compile_fail,E0277
/// // No Deserialize.
/// fn needs_de<T: serde::de::DeserializeOwned>() {}
/// needs_de::<driver::ObservedScreen>();
/// ```
#[derive(Clone)]
pub struct ObservedScreen {
    id: ObservationId,
    text: RedactedScreenText,
    apps: Arc<[AppId]>,
    flags: ObservationFlags,
}

impl ObservedScreen {
    pub(crate) fn from_observation(o: &Observation) -> Self {
        Self {
            id: o.id(),
            text: o.text().clone(),
            apps: Arc::from(o.apps()),
            flags: o.flags(),
        }
    }

    pub fn id(&self) -> ObservationId {
        self.id
    }

    pub fn text(&self) -> &RedactedScreenText {
        &self.text
    }

    pub fn apps(&self) -> &[AppId] {
        &self.apps
    }

    pub fn flags(&self) -> ObservationFlags {
        self.flags
    }
}

impl fmt::Debug for ObservedScreen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObservedScreen({:?}, {} apps)", self.id, self.apps.len())
    }
}
