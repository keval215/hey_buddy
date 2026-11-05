//! App-name resolution (ADR 0003 section 4): display name -> launch id / app id, deterministically.
//! The real Start-menu catalogue lives in a later ticket; this crate only has the trait and a
//! fixed fake.

use action::{AppId, LaunchId};

use crate::norm;

/// One installed, user-visible app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub launch: LaunchId,
    pub app: AppId,
}

pub trait Catalogue {
    /// `name` is already normalised (lowercase, accents folded, whitespace collapsed). Returns
    /// `None` when unknown OR ambiguous: the answer must be deterministic and never a guess.
    fn resolve(&self, name: &str) -> Option<AppEntry>;
}

/// Fixed in-memory catalogue (tests and fakes).
#[derive(Debug, Clone, Default)]
pub struct StaticCatalogue {
    /// (normalised display name, entry or None when the name is ambiguous)
    entries: Vec<(String, Option<AppEntry>)>,
}

impl StaticCatalogue {
    /// `(display name, launch id, app id)` triples. `None` if any id is invalid. A display name
    /// that normalises equal to an earlier one becomes ambiguous and resolves to nothing.
    pub fn new(items: &[(&str, &str, &str)]) -> Option<Self> {
        let mut entries: Vec<(String, Option<AppEntry>)> = Vec::new();
        for (display, launch, app) in items {
            let key = norm::provenance(display);
            if key.is_empty() {
                return None;
            }
            let entry = AppEntry {
                launch: LaunchId::new(launch).ok()?,
                app: AppId::parse(app)?,
            };
            if let Some(e) = entries.iter_mut().find(|(k, _)| *k == key) {
                e.1 = None;
            } else {
                entries.push((key, Some(entry)));
            }
        }
        Some(Self { entries })
    }
}

impl Catalogue for StaticCatalogue {
    fn resolve(&self, name: &str) -> Option<AppEntry> {
        self.entries
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, e)| e.clone())
    }
}
