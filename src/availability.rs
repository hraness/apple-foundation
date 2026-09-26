//! Why Apple's on-device model can't be used, and what the person can do.
//!
//! The bridge reports a reason code (`--check`, or `error.reason` on a
//! `modelUnavailable` response). This module keeps that code typed from the
//! bridge to the host and owns the plain-language copy for it, so every host
//! says the same thing and links the same System Settings pane.

use std::fmt;

/// `x-apple.systempreferences:` link to System Settings › Apple Intelligence & Siri.
pub const APPLE_INTELLIGENCE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Siri-Settings.extension";
/// Where the Apple Intelligence switch lives, as the person reads it.
pub const APPLE_INTELLIGENCE_SETTINGS_PATH: &str = "System Settings › Apple Intelligence & Siri";
/// `x-apple.systempreferences:` link to System Settings › General › Software Update.
pub const SOFTWARE_UPDATE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Software-Update-Settings.extension";
/// Where macOS updates live, as the person reads it.
pub const SOFTWARE_UPDATE_SETTINGS_PATH: &str = "System Settings › General › Software Update";

/// Why the on-device model is not usable. Wire names match the bridge's
/// `reason` strings (see `spec/protocol.md`); [`Reason::HelperMissing`] is
/// reported by the client when the bridge executable itself is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Reason {
    /// The Mac can't run Apple Intelligence (for example, an Intel Mac).
    DeviceNotEligible,
    /// Apple Intelligence is turned off in System Settings.
    AppleIntelligenceNotEnabled,
    /// Apple Intelligence is on, but the model is still downloading.
    ModelNotReady,
    /// macOS is older than 26.
    RequiresMacOS26,
    /// The bridge executable was not found.
    HelperMissing,
    /// The model is unavailable for a reason the system did not name, or a
    /// reason this version of the client does not know.
    Unavailable,
}

impl Reason {
    /// Every reason, in the order the copy table lists them.
    pub const ALL: [Reason; 6] = [
        Reason::AppleIntelligenceNotEnabled,
        Reason::ModelNotReady,
        Reason::DeviceNotEligible,
        Reason::RequiresMacOS26,
        Reason::HelperMissing,
        Reason::Unavailable,
    ];

    /// The wire name, e.g. `"appleIntelligenceNotEnabled"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeviceNotEligible => "deviceNotEligible",
            Self::AppleIntelligenceNotEnabled => "appleIntelligenceNotEnabled",
            Self::ModelNotReady => "modelNotReady",
            Self::RequiresMacOS26 => "requiresMacOS26",
            Self::HelperMissing => "helperMissing",
            Self::Unavailable => "unavailable",
        }
    }

    /// Parse a wire name. Unknown names become [`Reason::Unavailable`] so a
    /// newer bridge never turns "unavailable" into a protocol error.
    pub fn from_wire(name: &str) -> Self {
        match name {
            "deviceNotEligible" => Self::DeviceNotEligible,
            "appleIntelligenceNotEnabled" => Self::AppleIntelligenceNotEnabled,
            "modelNotReady" => Self::ModelNotReady,
            "requiresMacOS26" => Self::RequiresMacOS26,
            "helperMissing" => Self::HelperMissing,
            _ => Self::Unavailable,
        }
    }

    /// True when waiting fixes it without the person doing anything.
    pub fn is_temporary(self) -> bool {
        matches!(self, Self::ModelNotReady)
    }

    /// Plain-language copy for this reason, with the System Settings pane
    /// that fixes it when there is one.
    pub fn explain(self) -> Explanation {
        let (summary, fix, settings) = match self {
            Self::AppleIntelligenceNotEnabled => (
                "Apple Intelligence is off.",
                "Turn it on in System Settings › Apple Intelligence & Siri, then try again.",
                Some(Settings::AppleIntelligence),
            ),
            Self::ModelNotReady => (
                "Apple's on-device model is still downloading.",
                "Try again in a few minutes. System Settings › Apple Intelligence & Siri shows the progress.",
                Some(Settings::AppleIntelligence),
            ),
            Self::DeviceNotEligible => (
                "This Mac can't run Apple's on-device model. It needs Apple silicon and Apple Intelligence support.",
                "Use a different model on this Mac.",
                None,
            ),
            Self::RequiresMacOS26 => (
                "Apple's on-device model needs macOS 26 or later.",
                "Update macOS in System Settings › General › Software Update, then try again.",
                Some(Settings::SoftwareUpdate),
            ),
            Self::HelperMissing => (
                "The helper that talks to Apple's on-device model isn't installed.",
                "Install the helper, then try again.",
                None,
            ),
            Self::Unavailable => (
                "Apple's on-device model isn't available right now.",
                "Check System Settings › Apple Intelligence & Siri, then try again.",
                Some(Settings::AppleIntelligence),
            ),
        };
        let (settings_path, settings_url) = match settings {
            Some(Settings::AppleIntelligence) => (
                Some(APPLE_INTELLIGENCE_SETTINGS_PATH),
                Some(APPLE_INTELLIGENCE_SETTINGS_URL),
            ),
            Some(Settings::SoftwareUpdate) => (
                Some(SOFTWARE_UPDATE_SETTINGS_PATH),
                Some(SOFTWARE_UPDATE_SETTINGS_URL),
            ),
            None => (None, None),
        };
        Explanation {
            summary: summary.to_owned(),
            fix: fix.to_owned(),
            command: None,
            settings_path,
            settings_url,
            temporary: self.is_temporary(),
        }
    }
}

enum Settings {
    AppleIntelligence,
    SoftwareUpdate,
}

impl fmt::Display for Reason {
    /// The wire name. Use [`Reason::explain`] for text a person reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Human copy for a reason the model or its helper can't be used. Product
/// neutral: hosts add their own name and fallback ("so it's using the
/// built-in scorer") and may replace `fix`/`command` with their own install
/// step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    /// What is wrong, as a sentence.
    pub summary: String,
    /// The one thing to do about it, as a sentence.
    pub fix: String,
    /// A command that fixes it, when there is one (e.g. `xcode-select --install`).
    pub command: Option<&'static str>,
    /// The System Settings pane that fixes it, as the person reads it.
    pub settings_path: Option<&'static str>,
    /// `x-apple.systempreferences:` link that opens that pane.
    pub settings_url: Option<&'static str>,
    /// True when waiting is enough.
    pub temporary: bool,
}

impl fmt::Display for Explanation {
    /// `summary fix`, one line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.summary, self.fix)
    }
}

/// Result of [`crate::check`]. `reason` is `Some` exactly when `available` is
/// false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub available: bool,
    pub reason: Option<Reason>,
}

impl Availability {
    /// The model is ready.
    pub fn ready() -> Self {
        Self {
            available: true,
            reason: None,
        }
    }

    /// The model can't be used, for `reason`.
    pub fn unavailable(reason: Reason) -> Self {
        Self {
            available: false,
            reason: Some(reason),
        }
    }

    /// Copy for the person, or `None` when the model is ready.
    pub fn explain(&self) -> Option<Explanation> {
        if self.available {
            None
        } else {
            Some(self.reason.unwrap_or(Reason::Unavailable).explain())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Renders every reason's copy so a golden file pins each string.
    pub(crate) fn render(explanations: &[(&str, Explanation)]) -> String {
        let mut out = String::new();
        for (name, e) in explanations {
            out.push_str(&format!("[{name}]\n"));
            out.push_str(&format!("summary: {}\n", e.summary));
            out.push_str(&format!("fix: {}\n", e.fix));
            if let Some(command) = e.command {
                out.push_str(&format!("command: {command}\n"));
            }
            if let (Some(path), Some(url)) = (e.settings_path, e.settings_url) {
                out.push_str(&format!("settings: {path} <{url}>\n"));
            }
            if e.temporary {
                out.push_str("temporary: yes\n");
            }
            out.push('\n');
        }
        out
    }

    pub(crate) fn assert_golden(name: &str, actual: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, actual).unwrap();
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {path:?}: {e}; run with UPDATE_GOLDEN=1"));
        assert_eq!(
            actual, expected,
            "{name} changed; rerun with UPDATE_GOLDEN=1 and review the diff"
        );
    }

    #[test]
    fn wire_names_round_trip() {
        for reason in Reason::ALL {
            assert_eq!(Reason::from_wire(reason.as_str()), reason);
            assert_eq!(reason.to_string(), reason.as_str());
        }
        assert_eq!(Reason::from_wire("somethingNew"), Reason::Unavailable);
        assert_eq!(Reason::from_wire(""), Reason::Unavailable);
    }

    #[test]
    fn reason_copy_matches_golden() {
        let rows: Vec<(&str, Explanation)> = Reason::ALL
            .iter()
            .map(|r| (r.as_str(), r.explain()))
            .collect();
        assert_golden("reasons.txt", &render(&rows));
    }

    #[test]
    fn copy_is_plain_sentences_without_codes() {
        for reason in Reason::ALL {
            let e = reason.explain();
            for text in [&e.summary, &e.fix] {
                assert!(text.ends_with('.'), "{reason}: {text}");
                assert!(text.chars().next().unwrap().is_uppercase(), "{text}");
                assert!(!text.contains(reason.as_str()), "{text} leaks a code");
                assert!(text.len() <= 120, "{text} is too long for one line");
            }
            assert_eq!(e.settings_path.is_some(), e.settings_url.is_some());
            if let Some(path) = e.settings_path {
                assert!(
                    e.summary.contains(path) || e.fix.contains(path),
                    "{reason}: link without naming {path}"
                );
            }
            assert_eq!(e.temporary, reason.is_temporary());
        }
    }

    #[test]
    fn settings_links_are_apple_settings_urls() {
        for url in [
            APPLE_INTELLIGENCE_SETTINGS_URL,
            SOFTWARE_UPDATE_SETTINGS_URL,
        ] {
            assert!(url.starts_with("x-apple.systempreferences:com.apple."));
            assert!(!url.contains(' '));
        }
    }

    #[test]
    fn availability_explains_only_when_unavailable() {
        assert_eq!(Availability::ready().explain(), None);
        let off = Availability::unavailable(Reason::AppleIntelligenceNotEnabled);
        assert_eq!(
            off.explain().unwrap().settings_url,
            Some(APPLE_INTELLIGENCE_SETTINGS_URL)
        );
        let unnamed = Availability {
            available: false,
            reason: None,
        };
        assert_eq!(unnamed.explain(), Some(Reason::Unavailable.explain()));
        assert_eq!(
            off.explain().unwrap().to_string(),
            "Apple Intelligence is off. Turn it on in System Settings › Apple Intelligence & Siri, then try again."
        );
    }
}
