//! Pure deny-list policy shared by the real platform impls and the fake, so the logic is
//! testable on Linux.

use frame::WindowInfo;

use crate::{CaptureError, CaptureScope, DenyList};

/// Split windows into (kept, excluded) by the deny-list.
pub fn filter_windows(
    windows: &[WindowInfo],
    deny: &DenyList,
) -> (Vec<WindowInfo>, Vec<WindowInfo>) {
    let mut kept = Vec::new();
    let mut excluded = Vec::new();
    for w in windows {
        if deny.matches(&w.app_id) {
            excluded.push(w.clone());
        } else {
            kept.push(w.clone());
        }
    }
    (kept, excluded)
}

/// Outcome of the Windows policy (WGC cannot exclude windows from a monitor capture).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Safe to capture the whole display.
    CaptureDisplay,
    /// Capture only this (non-deny-listed) foreground window.
    CaptureForeground(WindowInfo),
    /// Do not capture.
    Refuse(CaptureError),
}

/// `visible`: visible windows on the display (our own windows already removed).
/// `foreground`: the foreground window, if known.
pub fn windows_policy(
    visible: &[WindowInfo],
    scope: CaptureScope,
    foreground: Option<&WindowInfo>,
    deny: &DenyList,
) -> Decision {
    match scope {
        CaptureScope::Display => match visible.iter().find(|w| deny.matches(&w.app_id)) {
            Some(w) => Decision::Refuse(CaptureError::DeniedAppVisible {
                app: w.app_id.clone(),
            }),
            None => Decision::CaptureDisplay,
        },
        CaptureScope::ForegroundWindow => match foreground {
            None => Decision::Refuse(CaptureError::Platform("no foreground window".into())),
            Some(w) if deny.matches(&w.app_id) => {
                Decision::Refuse(CaptureError::DeniedAppVisible {
                    app: w.app_id.clone(),
                })
            }
            Some(w) => Decision::CaptureForeground(w.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame::Rect;

    fn win(app: &str) -> WindowInfo {
        WindowInfo {
            app_id: app.into(),
            pid: 1,
            rect: Rect::new(0, 0, 10, 10),
        }
    }

    #[test]
    fn filter_splits() {
        let d = DenyList::defaults();
        let (k, e) = filter_windows(&[win("com.1password.1password"), win("com.example.ok")], &d);
        assert_eq!(k.len(), 1);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].app_id, "com.1password.1password");
    }

    #[test]
    fn denied_visible_refuses_display() {
        let d = DenyList::defaults();
        let v = [win("notepad.exe"), win("KeePassXC.exe")];
        assert_eq!(
            windows_policy(&v, CaptureScope::Display, None, &d),
            Decision::Refuse(CaptureError::DeniedAppVisible {
                app: "KeePassXC.exe".into()
            })
        );
    }

    #[test]
    fn clean_display_ok() {
        let d = DenyList::defaults();
        assert_eq!(
            windows_policy(&[win("notepad.exe")], CaptureScope::Display, None, &d),
            Decision::CaptureDisplay
        );
    }

    #[test]
    fn foreground_fallback() {
        let d = DenyList::defaults();
        let fg = win("notepad.exe");
        let v = [win("bitwarden.exe"), fg.clone()];
        assert_eq!(
            windows_policy(&v, CaptureScope::ForegroundWindow, Some(&fg), &d),
            Decision::CaptureForeground(fg)
        );
    }

    #[test]
    fn foreground_denied_or_missing_refuses() {
        let d = DenyList::defaults();
        let fg = win("WindowsTerminal.exe");
        assert!(matches!(
            windows_policy(
                std::slice::from_ref(&fg),
                CaptureScope::ForegroundWindow,
                Some(&fg),
                &d
            ),
            Decision::Refuse(CaptureError::DeniedAppVisible { .. })
        ));
        assert!(matches!(
            windows_policy(&[], CaptureScope::ForegroundWindow, None, &d),
            Decision::Refuse(CaptureError::Platform(_))
        ));
    }
}
