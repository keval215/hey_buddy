//! Strict `MAJOR.MINOR.PATCH` versions (no prerelease, no build metadata, no leading zeros).

use std::fmt;

use crate::UpdateError;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version {
    major: u16,
    minor: u16,
    patch: u16,
}

impl Version {
    /// Exactly three dot-separated decimal numbers, each 0..=65535 without leading zeros.
    pub fn parse(s: &str) -> Result<Self, UpdateError> {
        let mut parts = s.split('.');
        let mut next = || -> Result<u16, UpdateError> {
            let p = parts.next().ok_or(UpdateError::BadVersion)?;
            if p.is_empty()
                || p.len() > 5
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
            {
                return Err(UpdateError::BadVersion);
            }
            p.parse::<u16>().map_err(|_| UpdateError::BadVersion)
        };
        let v = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return Err(UpdateError::BadVersion);
        }
        Ok(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Stable,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        (s == "stable").then_some(Self::Stable)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Platform {
    WindowsX86_64,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        (s == "windows-x86_64").then_some(Self::WindowsX86_64)
    }

    /// The platform this binary runs on, if updates exist for it (Windows x86_64 only, D-005).
    pub fn current() -> Option<Self> {
        if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Self::WindowsX86_64)
        } else {
            None
        }
    }

    /// The `kind` an artifact of this platform must declare.
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "nsis",
        }
    }
}
