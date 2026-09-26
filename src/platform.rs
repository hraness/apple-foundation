//! Whether this Mac can run the bridge at all: macOS 26 or later on Apple
//! silicon. Probed with `/usr/sbin/sysctl`, which never prompts.

use crate::{Error, Reason, Result};
use std::process::{Command, Stdio};

/// Fail unless this is macOS. Everything that spawns a process calls this
/// first, so other platforms never start one.
pub(crate) fn os_guard() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "Apple Foundation Models requires macOS".into(),
        ))
    }
}

/// Check that this Mac can run Apple's on-device model: macOS 26 or later on
/// Apple silicon.
///
/// - Not macOS: [`Error::Unsupported`].
/// - Intel Mac: [`Error::Unavailable`] with [`Reason::DeviceNotEligible`].
/// - macOS before 26: [`Error::Unavailable`] with [`Reason::RequiresMacOS26`].
///
/// If the probe itself can't run, this returns `Ok(())` and leaves the answer
/// to the bridge's `--check`. It never starts the bridge or a compiler, and it
/// never causes a system prompt. An Apple silicon Mac that passes can still be
/// ineligible or have Apple Intelligence turned off; [`crate::check`] reports
/// those.
pub fn platform_check() -> Result<()> {
    os_guard()?;
    match platform_reason(&probe()) {
        Some(reason) => Err(Error::Unavailable(reason)),
        None => Ok(()),
    }
}

/// `sysctl kern.osproductversion hw.optional.arm64` output. The arm64 key is
/// missing on Intel Macs, so sysctl exits 1 there but still prints the
/// version line; the exit status is ignored on purpose.
fn probe() -> String {
    Command::new("/usr/sbin/sysctl")
        .args(["kern.osproductversion", "hw.optional.arm64"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Reason this Mac can't run the model, from sysctl output. `hw.optional.arm64`
/// is 1 on Apple silicon, including for x86_64 processes under Rosetta.
/// Unparseable output answers `None`: the probe must not block a Mac it
/// doesn't understand.
pub(crate) fn platform_reason(sysctl: &str) -> Option<Reason> {
    let mut version = None;
    let mut arm64 = false;
    for line in sysctl.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "kern.osproductversion" => {
                version = value
                    .trim()
                    .split('.')
                    .next()
                    .and_then(|major| major.parse::<u32>().ok());
            }
            "hw.optional.arm64" => arm64 = value.trim() == "1",
            _ => {}
        }
    }
    let major = version?;
    if !arm64 {
        return Some(Reason::DeviceNotEligible);
    }
    if major < 26 {
        return Some(Reason::RequiresMacOS26);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_silicon_on_macos_26_passes() {
        let out = "kern.osproductversion: 26.5.2\nhw.optional.arm64: 1\n";
        assert_eq!(platform_reason(out), None);
        assert_eq!(
            platform_reason("kern.osproductversion: 27.0\nhw.optional.arm64: 1\n"),
            None
        );
    }

    #[test]
    fn older_macos_needs_26() {
        let out = "kern.osproductversion: 15.6.1\nhw.optional.arm64: 1\n";
        assert_eq!(platform_reason(out), Some(Reason::RequiresMacOS26));
    }

    #[test]
    fn intel_is_not_eligible_even_on_old_macos() {
        // The arm64 key is absent on Intel; updating macOS would not help.
        assert_eq!(
            platform_reason("kern.osproductversion: 15.6\n"),
            Some(Reason::DeviceNotEligible)
        );
        assert_eq!(
            platform_reason("kern.osproductversion: 26.0\nhw.optional.arm64: 0\n"),
            Some(Reason::DeviceNotEligible)
        );
    }

    #[test]
    fn unreadable_probe_does_not_block() {
        assert_eq!(platform_reason(""), None);
        assert_eq!(platform_reason("garbage"), None);
        assert_eq!(
            platform_reason("kern.osproductversion: x\nhw.optional.arm64: 1\n"),
            None
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_are_unsupported() {
        assert!(matches!(platform_check(), Err(Error::Unsupported(_))));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_probe_answers_without_prompting() {
        // Whatever this Mac is, the answer is typed: ok or a platform reason.
        match platform_check() {
            Ok(()) => {}
            Err(Error::Unavailable(Reason::RequiresMacOS26 | Reason::DeviceNotEligible)) => {}
            other => panic!("unexpected platform answer: {other:?}"),
        }
    }
}
