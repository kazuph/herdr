//! Update support for the kazuph/herdr fork.
//!
//! This fork intentionally has no network self-update path. Build from source
//! and replace the local binary explicitly when updating.

pub(crate) const UPDATE_TRACK_ID: &str = "kazuph/herdr";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Self {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub(crate) fn update_install_command() -> &'static str {
    "build from source and install target/release/herdr"
}

pub fn self_update() -> Result<Version, String> {
    Err("self-update is disabled in the kazuph/herdr fork; build from source and install target/release/herdr explicitly".into())
}

pub fn auto_update(_events: tokio::sync::mpsc::Sender<crate::events::AppEvent>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_accepts_plain_and_prefixed_semver() {
        assert_eq!(
            Version::parse("1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
            })
        );
        assert_eq!(Version::parse("v0.1.0"), Version::parse("0.1.0"));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("abc"), None);
    }

    #[test]
    fn self_update_is_disabled_for_fork() {
        let err = self_update().expect_err("fork self-update should be disabled");
        assert!(err.contains("disabled"));
    }
}
