//! Building the bridge from the embedded Swift source, without surprises.
//!
//! On a Mac without Apple's command line tools, `/usr/bin/xcrun`, `swiftc`
//! and `git` are stubs that open the "install command line developer tools"
//! dialog. `/usr/bin/xcode-select -p` does not, so it is always asked first,
//! and nothing else runs unless it names an existing developer directory.
//! Compiler output is captured, never inherited: only a short tail reaches
//! the host, inside [`Error::BuildFailed`].

use crate::{
    platform_check, Error, Explanation, Reason, Result, SOFTWARE_UPDATE_SETTINGS_PATH,
    SOFTWARE_UPDATE_SETTINGS_URL, SWIFT_SOURCE,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Oldest macOS SDK that has `FoundationModels`.
const MIN_SDK_MAJOR: u32 = 26;
/// Lines of compiler output kept in [`Error::BuildFailed`].
const TAIL_LINES: usize = 8;
/// Bytes of compiler output kept in [`Error::BuildFailed`].
const TAIL_BYTES: usize = 2_048;

/// Why the bridge can't be built on this Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolsProblem {
    /// `xcode-select -p` names no developer directory: Apple's command line
    /// tools and Xcode are both missing.
    NotInstalled,
    /// Developer tools exist, but `xcrun` can't find `swiftc`.
    NoSwiftCompiler,
    /// The selected macOS SDK predates 26, so it has no `FoundationModels`.
    SdkTooOld {
        /// The SDK version `xcrun` reported, e.g. `"15.4"`.
        version: String,
    },
}

impl ToolsProblem {
    /// Plain-language copy, with the command or pane that fixes it.
    pub fn explain(&self) -> Explanation {
        match self {
            Self::NotInstalled => Explanation {
                summary: "Apple's command line tools aren't installed, so the Apple model helper can't be built.".into(),
                fix: "Install them with xcode-select --install, then try again. Nothing was installed.".into(),
                command: Some("xcode-select --install"),
                settings_path: None,
                settings_url: None,
                temporary: false,
            },
            Self::NoSwiftCompiler => Explanation {
                summary: "Apple's command line tools on this Mac don't include the Swift compiler.".into(),
                fix: "Reinstall them with xcode-select --install, or install Xcode 26 or later.".into(),
                command: Some("xcode-select --install"),
                settings_path: None,
                settings_url: None,
                temporary: false,
            },
            Self::SdkTooOld { version } => Explanation {
                summary: format!(
                    "The Apple model helper needs the macOS 26 SDK, but the selected tools have macOS {version}."
                ),
                fix: "Update Xcode or the command line tools in System Settings › General › Software Update.".into(),
                command: None,
                settings_path: Some(SOFTWARE_UPDATE_SETTINGS_PATH),
                settings_url: Some(SOFTWARE_UPDATE_SETTINGS_URL),
                temporary: false,
            },
        }
    }
}

impl fmt::Display for ToolsProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("Apple's command line tools aren't installed"),
            Self::NoSwiftCompiler => f.write_str("the Swift compiler isn't installed"),
            Self::SdkTooOld { version } => {
                write!(f, "the macOS {version} SDK is older than macOS 26")
            }
        }
    }
}

/// Copy for [`Error::BuildFailed`].
pub(crate) fn build_failed_explanation() -> Explanation {
    Explanation {
        summary: "The Apple model helper didn't build.".into(),
        fix: "Check that Xcode 26 or later is selected (xcode-select -p), then try again.".into(),
        command: None,
        settings_path: None,
        settings_url: None,
        temporary: false,
    }
}

/// The developer tools the build needs, behind a seam so tests never run a
/// real compiler or risk the install dialog.
pub(crate) trait Toolchain {
    /// `xcode-select -p`, only when it names an existing directory.
    fn developer_dir(&self) -> Option<PathBuf>;
    /// `xcrun --find swiftc`. Called only after `developer_dir` succeeds.
    fn find_swiftc(&self) -> Option<PathBuf>;
    /// `xcrun --sdk macosx --show-sdk-version`, e.g. `"26.2"`.
    fn sdk_version(&self) -> Option<String>;
    /// Compile `source` to `output` with every stream captured.
    fn compile(&self, source: &Path, output: &Path) -> std::io::Result<Output>;
}

pub(crate) struct SystemToolchain;

fn quiet_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

impl Toolchain for SystemToolchain {
    fn developer_dir(&self) -> Option<PathBuf> {
        let dir = PathBuf::from(quiet_stdout("/usr/bin/xcode-select", &["-p"])?);
        // A selected directory that was deleted makes xcrun offer the
        // install dialog again, so it counts as missing.
        dir.is_dir().then_some(dir)
    }

    fn find_swiftc(&self) -> Option<PathBuf> {
        let path = PathBuf::from(quiet_stdout("/usr/bin/xcrun", &["--find", "swiftc"])?);
        path.is_file().then_some(path)
    }

    fn sdk_version(&self) -> Option<String> {
        quiet_stdout("/usr/bin/xcrun", &["--sdk", "macosx", "--show-sdk-version"])
    }

    fn compile(&self, source: &Path, output: &Path) -> std::io::Result<Output> {
        Command::new("/usr/bin/xcrun")
            .args([
                "--sdk",
                "macosx",
                "swiftc",
                "-parse-as-library",
                "-O",
                "-target",
                "arm64-apple-macosx26.0",
            ])
            .arg(source)
            .arg("-o")
            .arg(output)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    }
}

/// Check that this Mac can build the bridge, without opening the developer
/// tools install dialog. Returns `Ok(())` when [`crate::ensure_bridge`] would
/// be able to compile. Hosts call this first to decide whether to warn that
/// macOS will offer to install the tools.
pub fn build_tools_check() -> std::result::Result<(), ToolsProblem> {
    tools_check_with(&SystemToolchain)
}

pub(crate) fn tools_check_with(tools: &dyn Toolchain) -> std::result::Result<(), ToolsProblem> {
    tools.developer_dir().ok_or(ToolsProblem::NotInstalled)?;
    tools.find_swiftc().ok_or(ToolsProblem::NoSwiftCompiler)?;
    if let Some(version) = tools.sdk_version() {
        let major = version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok());
        if major.is_some_and(|m| m < MIN_SDK_MAJOR) {
            return Err(ToolsProblem::SdkTooOld { version });
        }
    }
    Ok(())
}

/// Stamp written next to a built bridge recording which source it came from.
/// A version bump or source edit invalidates older installs.
fn source_stamp() -> String {
    format!("{}:{}", env!("CARGO_PKG_VERSION"), SWIFT_SOURCE.len())
}

/// True when `install` holds a bridge built from this crate's source, so
/// [`crate::ensure_bridge`] would return at once without compiling. Hosts use
/// it to decide whether to print "Building the Apple model helper…" first.
pub fn bridge_is_current(install: &Path) -> bool {
    install.is_file()
        && std::fs::read_to_string(install.with_extension("stamp"))
            .is_ok_and(|s| s == source_stamp())
}

pub(crate) fn ensure_bridge_with(install: &Path, tools: &dyn Toolchain) -> Result<PathBuf> {
    if bridge_is_current(install) {
        return Ok(install.to_path_buf());
    }
    // Don't build what this Mac can't run.
    platform_check()?;
    tools_check_with(tools).map_err(Error::ToolsMissing)?;
    let dir = install
        .parent()
        .ok_or_else(|| Error::Protocol("install path has no parent".into()))?;
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let src = std::env::temp_dir().join(format!("apple-bridge-{}.swift", std::process::id()));
    std::fs::write(&src, SWIFT_SOURCE).map_err(Error::Io)?;
    let tmp_out = dir.join(format!(".apple-bridge-{}.tmp", std::process::id()));
    let compiled = tools.compile(&src, &tmp_out);
    let _ = std::fs::remove_file(&src);
    let output = compiled.map_err(Error::Spawn)?;
    if !output.status.success() || !tmp_out.is_file() {
        let _ = std::fs::remove_file(&tmp_out);
        return Err(Error::BuildFailed {
            status: output.status.code(),
            log_tail: tail(&output.stderr, &output.stdout),
        });
    }
    std::fs::rename(&tmp_out, install).map_err(Error::Io)?;
    let _ = std::fs::write(install.with_extension("stamp"), source_stamp());
    Ok(install.to_path_buf())
}

/// Last non-empty lines of compiler output (stderr first, then stdout),
/// bounded in lines and bytes.
pub(crate) fn tail(stderr: &[u8], stdout: &[u8]) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stderr),
        String::from_utf8_lossy(stdout)
    );
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut kept: Vec<&str> = lines[lines.len().saturating_sub(TAIL_LINES)..].to_vec();
    while kept.iter().map(|l| l.len() + 1).sum::<usize>() > TAIL_BYTES && kept.len() > 1 {
        kept.remove(0);
    }
    let mut out = kept.join("\n");
    if out.len() > TAIL_BYTES {
        let mut cut = out.len() - TAIL_BYTES;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        out = format!("…{}", &out[cut..]);
    }
    out
}

impl From<ToolsProblem> for Reason {
    /// A host that only tracks reasons sees "the helper isn't there".
    fn from(_: ToolsProblem) -> Self {
        Reason::HelperMissing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    #[derive(Default)]
    struct FakeTools {
        developer_dir: bool,
        swiftc: bool,
        sdk: Option<&'static str>,
        compile_fails: bool,
        compiled: Cell<u32>,
        asked_swiftc: Cell<bool>,
    }

    impl Toolchain for FakeTools {
        fn developer_dir(&self) -> Option<PathBuf> {
            self.developer_dir
                .then(|| PathBuf::from("/Applications/Xcode.app/Contents/Developer"))
        }
        fn find_swiftc(&self) -> Option<PathBuf> {
            self.asked_swiftc.set(true);
            self.swiftc.then(|| PathBuf::from("/fake/swiftc"))
        }
        fn sdk_version(&self) -> Option<String> {
            self.sdk.map(str::to_owned)
        }
        fn compile(&self, source: &Path, output: &Path) -> std::io::Result<Output> {
            self.compiled.set(self.compiled.get() + 1);
            assert!(std::fs::read_to_string(source)?.contains("FoundationModels"));
            if self.compile_fails {
                let mut stderr = String::new();
                for i in 1..=20 {
                    stderr.push_str(&format!("note: line {i}\n"));
                }
                stderr.push_str("error: no such module 'FoundationModels'\n");
                return Ok(Output {
                    status: ExitStatus::from_raw(1 << 8),
                    stdout: b"stdout noise\n".to_vec(),
                    stderr: stderr.into_bytes(),
                });
            }
            std::fs::write(output, b"#!/bin/sh\n")?;
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    fn ready() -> FakeTools {
        FakeTools {
            developer_dir: true,
            swiftc: true,
            sdk: Some("26.2"),
            ..FakeTools::default()
        }
    }

    #[cfg(target_os = "macos")]
    struct Scratch(PathBuf);
    #[cfg(target_os = "macos")]
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "apple-foundation-build-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
        fn install(&self) -> PathBuf {
            self.0.join("bin/apple-bridge")
        }
    }
    #[cfg(target_os = "macos")]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_tools_never_reach_xcrun() {
        let tools = FakeTools::default();
        assert_eq!(tools_check_with(&tools), Err(ToolsProblem::NotInstalled));
        assert!(
            !tools.asked_swiftc.get(),
            "xcrun must not run without tools"
        );
    }

    #[test]
    fn missing_swiftc_and_old_sdk_are_typed() {
        let tools = FakeTools {
            developer_dir: true,
            ..FakeTools::default()
        };
        assert_eq!(tools_check_with(&tools), Err(ToolsProblem::NoSwiftCompiler));
        let tools = FakeTools {
            sdk: Some("15.4"),
            ..ready()
        };
        assert_eq!(
            tools_check_with(&tools),
            Err(ToolsProblem::SdkTooOld {
                version: "15.4".into()
            })
        );
        // An unreadable SDK version doesn't block; swiftc will say.
        let tools = FakeTools {
            sdk: None,
            ..ready()
        };
        assert!(tools_check_with(&tools).is_ok());
    }

    #[cfg(target_os = "macos")]
    fn platform_ok() -> bool {
        platform_check().is_ok()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_returns_tools_missing_without_compiling() {
        let scratch = Scratch::new("missing");
        let tools = FakeTools::default();
        match ensure_bridge_with(&scratch.install(), &tools) {
            Err(Error::ToolsMissing(ToolsProblem::NotInstalled)) => assert!(platform_ok()),
            Err(Error::Unavailable(_)) => assert!(!platform_ok()),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(tools.compiled.get(), 0);
        assert!(!scratch.install().exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_build_keeps_only_a_bounded_tail() {
        if !platform_ok() {
            return; // CI runs macOS 14; the platform check answers first there.
        }
        let scratch = Scratch::new("fails");
        let tools = FakeTools {
            compile_fails: true,
            ..ready()
        };
        let error = ensure_bridge_with(&scratch.install(), &tools).unwrap_err();
        let Error::BuildFailed { status, log_tail } = &error else {
            panic!("expected BuildFailed, got {error:?}");
        };
        assert_eq!(*status, Some(1));
        assert_eq!(log_tail.lines().count(), TAIL_LINES);
        assert!(log_tail.contains("no such module 'FoundationModels'"));
        assert!(log_tail.ends_with("stdout noise"));
        assert!(!log_tail.contains("note: line 1\n"));
        assert_eq!(
            error.to_string(),
            "couldn't build the Apple model helper (swiftc exited with status 1)"
        );
        assert_eq!(error.reason(), Some(Reason::HelperMissing));
        assert!(!scratch.install().exists());
        assert!(std::fs::read_dir(scratch.install().parent().unwrap())
            .unwrap()
            .next()
            .is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn successful_build_is_stamped_and_not_repeated() {
        if !platform_ok() {
            return;
        }
        let scratch = Scratch::new("builds");
        let tools = ready();
        assert!(!bridge_is_current(&scratch.install()));
        ensure_bridge_with(&scratch.install(), &tools).unwrap();
        assert!(bridge_is_current(&scratch.install()));
        ensure_bridge_with(&scratch.install(), &tools).unwrap();
        assert_eq!(tools.compiled.get(), 1);
        // A current bridge needs no tools at all.
        ensure_bridge_with(&scratch.install(), &FakeTools::default()).unwrap();
    }

    #[test]
    fn tail_is_bounded_in_bytes_and_utf8_safe() {
        let long = "é".repeat(5_000);
        let t = tail(long.as_bytes(), b"");
        assert!(t.len() <= TAIL_BYTES + '…'.len_utf8());
        assert!(t.starts_with('…'));
        assert_eq!(tail(b"\n\n", b""), "");
    }

    #[test]
    fn tools_copy_matches_golden() {
        let rows = [
            ("notInstalled", ToolsProblem::NotInstalled.explain()),
            ("noSwiftCompiler", ToolsProblem::NoSwiftCompiler.explain()),
            (
                "sdkTooOld(15.4)",
                ToolsProblem::SdkTooOld {
                    version: "15.4".into(),
                }
                .explain(),
            ),
            ("buildFailed", build_failed_explanation()),
        ];
        crate::availability::tests::assert_golden(
            "tools.txt",
            &crate::availability::tests::render(&rows),
        );
        for (_, e) in &rows {
            for text in [&e.summary, &e.fix] {
                assert!(text.ends_with('.') && text.len() <= 120, "{text}");
            }
        }
    }
}
