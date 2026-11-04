//! Pure data types shared by capture, firewall and llm. No platform dependencies.
//!
//! `Frame` deliberately is not `Clone` and has no `Deserialize`: the only way to obtain one is
//! `Frame::new` (validated) and the only way to give pixels away is by consuming it.

mod frame;
mod geometry;
mod policy;
mod window;

pub use frame::{Frame, FrameError, FrameParts};
pub use geometry::{DisplayId, DisplayInfo, Rect};
pub use policy::AppDenyPolicy;
pub use window::WindowInfo;
