/// Decides whether an application must never be read or captured (password managers, banking
/// apps, ...). Implemented by `capture::DenyList`; consumed by the firewall's text path so the
/// `firewall` crate does not depend on `capture`.
pub trait AppDenyPolicy: Send + Sync {
    /// `app_id` is a bundle id / executable name, never a window title. Matching is the
    /// implementor's business (the capture deny list is case-insensitive).
    fn is_denied(&self, app_id: &str) -> bool;
}
