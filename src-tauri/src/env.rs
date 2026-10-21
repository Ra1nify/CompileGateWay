//! Target-machine environment detection (design doc §8).
//!
//! Goal: one click tells us whether this machine can build the project —
//! which Xcode is installed, the JDK / Gradle / Flutter / Rust versions,
//! and how much disk is left.
//!
//! Deliberately runs only one or two SSH calls (not one round trip per probe):
//! all probes are joined into a single POSIX shell script, each section
//! prefixed with a `###cgw:<id>` marker, split locally on return. Probes are
//! guarded by `command -v`, printing `__CGW_MISSING__` when the tool is absent,
//! so one missing tool never fails the whole detection.
//!
//! The Xcode instance scan must run on the target (only it knows the paths) and
//! is embedded in the same script; the script picks the platform via `uname -s`
//! and only Darwin enters that loop.

use std::time::Duration;

use crate::clients::Client;
use crate::ssh::{Cancel, Session, shell_path};

/// Which probe this section belongs to.
pub const SECTION_MARK: &str = "###cgw:";
/// Tool is not on PATH.
pub const MISSING_MARK: &str = "__CGW_MISSING__";
/// Separator between Xcode instances.
const INSTANCE_MARK: &str = "__CGW_INSTANCE__";

/// Overall detection timeout (`flutter --version` can be slow on first run).
pub const ENV_TIMEOUT: Duration = Duration::from_secs(90);

/// One probe: what to check and how to check it.
pub struct Probe {
    pub id: &'static str,
    /// Display name in the UI.
    pub label: &'static str,
    /// Grouping key (design doc §8: group by the `platform` field).
    pub platform: &'static str,
    /// Executable name used for the `command -v` guard.
    pub binary: &'static str,
    /// Command that yields the version (may contain pipes).
    pub command: &'static str,
}

pub const PLATFORM_XCODE: &str = "xcode";
pub const PLATFORM_GRADLE: &str = "gradle";
pub const PLATFORM_FLUTTER: &str = "flutter";
pub const PLATFORM_DART: &str = "dart";
pub const PLATFORM_RUST: &str = "rust";
pub const PLATFORM_COMMON: &str = "common";

/// Default v1 probe list (design doc §8). Add one entry here for a new platform.
pub const PROBES: &[Probe] = &[
    Probe {
        id: "xcodebuild",
        label: "Xcode",
        platform: PLATFORM_XCODE,
        binary: "xcodebuild",
        command: "xcodebuild -version",
    },
    Probe {
        id: "cocoapods",
        label: "CocoaPods",
        platform: PLATFORM_XCODE,
        binary: "pod",
        command: "pod --version",
    },
    Probe {
        id: "homebrew",
        label: "Homebrew",
        platform: PLATFORM_XCODE,
        binary: "brew",
        command: "brew --version",
    },
    Probe {
        id: "java",
        label: "JDK",
        platform: PLATFORM_GRADLE,
        binary: "java",
        command: "java -version",
    },
    Probe {
        id: "gradle",
        label: "Gradle",
        platform: PLATFORM_GRADLE,
        binary: "gradle",
        command: "gradle --version",
    },
    Probe {
        id: "flutter",
        label: "Flutter",
        platform: PLATFORM_FLUTTER,
        binary: "flutter",
        command: "flutter --version",
    },
    Probe {
        id: "dart",
        label: "Dart",
        platform: PLATFORM_DART,
        binary: "dart",
        command: "dart --version",
    },
    Probe {
        id: "cargo",
        label: "Cargo",
        platform: PLATFORM_RUST,
        binary: "cargo",
        command: "cargo --version",
    },
    Probe {
        id: "rustc",
        label: "rustc",
        platform: PLATFORM_RUST,
        binary: "rustc",
        command: "rustc --version",
    },
    Probe {
        id: "rsync",
        label: "rsync",
        platform: PLATFORM_COMMON,
        binary: "rsync",
        command: "rsync --version",
    },
    Probe {
        id: "xcodebuildmcp",
        label: "XcodeBuildMCP",
        platform: PLATFORM_COMMON,
        binary: "xcodebuildmcp",
        command: "xcodebuildmcp --help",
    },
];

// ------------------------------------------------------------------ report model

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub label: String,
    pub platform: String,
    /// Command actually executed (lets the user re-check on the target).
    pub command: String,
    pub found: bool,
    /// Extracted version; empty when none could be read.
    pub version: String,
    /// Raw output (truncated to a few lines).
    pub output: String,
}

/// One Xcode install (design doc §8: scan `/Applications/*.app` for `Xcode*.app`).
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XcodeInstance {
    /// Full path of the `.app`.
    pub path: String,
    /// This instance's own version.
    pub version: String,
    /// This instance's own build number.
    pub build: String,
    /// Value to pass as `DEVELOPER_DIR` when building.
    pub developer_dir: String,
    pub output: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub mount: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    /// 0.0 - 1.0
    pub used_ratio: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvReport {
    pub client_id: String,
    pub client_name: String,
    pub ok: bool,
    pub message: String,
    /// uname -s
    pub os: String,
    /// Time detection finished (unix milliseconds).
    pub checked_at: u64,
    pub elapsed_ms: u64,
    pub items: Vec<Item>,
    pub xcodes: Vec<XcodeInstance>,
    /// Raw output of `xcodebuild -license status` (empty off macOS).
    pub xcode_license: String,
    pub license_ok: bool,
    pub disk: Option<Disk>,
    /// Display structure after grouping by platform (used directly by the UI).
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub platform: String,
    pub items: Vec<Item>,
    /// Whether any probe in this group was found.
    pub available: bool,
}

impl EnvReport {
    fn regroup(&mut self) {
        let mut order: Vec<String> = Vec::new();
        for probe in PROBES {
            if !order.iter().any(|p| p == probe.platform) {
                order.push(probe.platform.to_string());
            }
        }
        self.groups = order
            .into_iter()
            .map(|platform| {
                let items: Vec<Item> = self
                    .items
                    .iter()
                    .filter(|item| item.platform == platform)
                    .cloned()
                    .collect();
                Group {
                    available: items.iter().any(|item| item.found),
                    platform,
                    items,
                }
            })
            .filter(|group| !group.items.is_empty())
            .collect();
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

// ------------------------------------------------------------------ script

/// Build the one and only detection script.
///
/// Public so the script itself can be unit-tested — command assembly is the
/// easiest place to introduce injection problems.
pub fn probe_script() -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("printf '%s\\n' '{SECTION_MARK}os'"));
    lines.push("uname -s 2>/dev/null || true".to_string());

    for probe in PROBES {
        lines.push(format!("printf '%s\\n' '{SECTION_MARK}{}'", probe.id));
        lines.push(format!(
            "if command -v {binary} >/dev/null 2>&1; then {command} 2>&1 | head -n 6; else printf '%s\\n' '{MISSING_MARK}'; fi",
            binary = probe.binary,
            command = probe.command,
        ));
    }

    // Disk: df -Pk guarantees one line per filesystem
    lines.push(format!("printf '%s\\n' '{SECTION_MARK}disk'"));
    lines.push("df -Pk \"$HOME\" 2>/dev/null | tail -n 1 || true".to_string());

    // Xcode instances: only Darwin has /Applications
    lines.push(format!("printf '%s\\n' '{SECTION_MARK}xcode'"));
    lines.push("if [ \"$(uname -s 2>/dev/null)\" = \"Darwin\" ]; then".to_string());
    lines.push("  for app in /Applications/Xcode*.app; do".to_string());
    lines.push("    [ -d \"$app\" ] || continue".to_string());
    lines.push(format!("    printf '%s\\t%s\\n' '{INSTANCE_MARK}' \"$app\""));
    lines.push(
        "    DEVELOPER_DIR=\"$app/Contents/Developer\" xcodebuild -version 2>&1 | head -n 4"
            .to_string(),
    );
    lines.push("  done".to_string());
    lines.push("fi".to_string());

    // Xcode license status (explicitly required by design doc §8)
    lines.push(format!("printf '%s\\n' '{SECTION_MARK}license'"));
    lines.push("if command -v xcodebuild >/dev/null 2>&1; then".to_string());
    lines.push("  xcodebuild -license status 2>&1 | head -n 4".to_string());
    lines.push("fi".to_string());

    lines.join("\n")
}

// ------------------------------------------------------------------ parsing

/// Split script output into `id -> raw text`.
pub fn split_sections(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(id) = line.strip_prefix(SECTION_MARK) {
            out.push((id.trim().to_string(), String::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            if !last.1.is_empty() {
                last.1.push('\n');
            }
            last.1.push_str(line);
        }
    }
    out
}

/// Extract a version from one tool's output: prefer dotted numeric tokens like
/// `1.2` / `1.2.3`.
pub fn extract_version(output: &str) -> String {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == MISSING_MARK {
            continue;
        }
        if let Some(version) = first_version_token(trimmed) {
            return version;
        }
    }
    String::new()
}

fn first_version_token(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        // Starts with a digit: consume [0-9.] forward
        let start = index;
        let mut end = index;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        let token = line[start..end].trim_end_matches('.');
        // Must look like `1.2`; a bare `3` is too easy to false-positive (e.g. "3 files")
        if token.contains('.') && !token.starts_with('.') && !token.ends_with('.') {
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
                return Some(token.to_string());
            }
        }
        index = end.max(index + 1);
    }
    None
}

fn parse_disk(text: &str) -> Option<Disk> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }
    let blocks: u64 = fields[1].parse().ok()?;
    let used: u64 = fields[2].parse().ok()?;
    let available: u64 = fields[3].parse().ok()?;
    let total = blocks * 1024;
    Some(Disk {
        mount: if fields.len() > 5 {
            fields[5..].join(" ")
        } else {
            fields[0].to_string()
        },
        total_bytes: total,
        used_bytes: used * 1024,
        available_bytes: available * 1024,
        used_ratio: if blocks == 0 {
            0.0
        } else {
            (used as f64 / blocks as f64).clamp(0.0, 1.0)
        },
    })
}

/// The Xcode section: `__CGW_INSTANCE__\t<path>` followed by that instance's
/// `xcodebuild -version`.
pub fn parse_xcodes(text: &str) -> Vec<XcodeInstance> {
    let mut out: Vec<XcodeInstance> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix(INSTANCE_MARK) {
            let path = path.trim_start_matches('\t').trim().to_string();
            out.push(XcodeInstance {
                developer_dir: format!("{}/Contents/Developer", path.trim_end_matches('/')),
                path,
                ..XcodeInstance::default()
            });
            continue;
        }
        if let Some(last) = out.last_mut() {
            if !last.output.is_empty() {
                last.output.push('\n');
            }
            last.output.push_str(line);
        }
    }
    for instance in out.iter_mut() {
        instance.version = extract_version(&instance.output);
        instance.build = instance
            .output
            .lines()
            .find_map(|line| line.trim().strip_prefix("Build version"))
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
    }
    out
}

fn parse_license(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.trim().is_empty() {
        return false;
    }
    // Already accepted / not needed: cover the wording on both platforms
    lower.contains("has been accepted")
        || lower.contains("already accepted")
        || lower.contains("license has been")
        || lower.contains("license is accepted")
}

/// Assemble the full report from script output.
pub fn parse_report(client: &Client, text: &str, elapsed_ms: u64) -> EnvReport {
    let sections = split_sections(text);
    let section = |id: &str| -> String {
        sections
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, body)| body.clone())
            .unwrap_or_default()
    };

    let items: Vec<Item> = PROBES
        .iter()
        .map(|probe| {
            let body = section(probe.id);
            let found = !body.trim().is_empty() && !body.contains(MISSING_MARK);
            Item {
                id: probe.id.to_string(),
                label: probe.label.to_string(),
                platform: probe.platform.to_string(),
                command: probe.command.to_string(),
                found,
                version: if found { extract_version(&body) } else { String::new() },
                output: body.trim().to_string(),
            }
        })
        .collect();

    let xcodes = parse_xcodes(&section("xcode"));
    let license = section("license");
    let os = section("os").trim().to_string();

    let mut report = EnvReport {
        client_id: client.id.clone(),
        client_name: client.name.clone(),
        ok: true,
        message: String::new(),
        os,
        checked_at: crate::projects::now_ms(),
        elapsed_ms,
        items,
        xcodes,
        license_ok: parse_license(&license),
        xcode_license: license.trim().to_string(),
        disk: parse_disk(&section("disk")),
        groups: Vec::new(),
    };
    report.regroup();
    report
}

// ------------------------------------------------------------------ execution

/// Connect and run one full detection.
pub async fn detect(client: &Client, cancel: &Cancel) -> EnvReport {
    let started = std::time::Instant::now();

    let mut session = match Session::open_cancellable(client, cancel).await {
        Ok(session) => session,
        Err(message) => {
            return EnvReport {
                client_id: client.id.clone(),
                client_name: client.name.clone(),
                ok: false,
                message,
                checked_at: crate::projects::now_ms(),
                ..EnvReport::default()
            }
        }
    };

    let script = probe_script();
    let exec = match tokio::time::timeout(
        ENV_TIMEOUT,
        session.exec_collect_cancellable(&script, cancel),
    )
    .await
    {
        Ok(Ok(exec)) => exec,
        Ok(Err(message)) => {
            return EnvReport {
                client_id: client.id.clone(),
                client_name: client.name.clone(),
                ok: false,
                message,
                checked_at: crate::projects::now_ms(),
                ..EnvReport::default()
            }
        }
        Err(_) => {
            return EnvReport {
                client_id: client.id.clone(),
                client_name: client.name.clone(),
                ok: false,
                message: crate::i18n::env_timeout(
                    crate::i18n::current(),
                    ENV_TIMEOUT.as_secs(),
                ),
                checked_at: crate::projects::now_ms(),
                ..EnvReport::default()
            }
        }
    };

    if exec.cancelled {
        return EnvReport {
            client_id: client.id.clone(),
            client_name: client.name.clone(),
            ok: false,
            message: crate::i18n::env_cancelled(crate::i18n::current()),
            checked_at: crate::projects::now_ms(),
            ..EnvReport::default()
        };
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let mut report = parse_report(client, &exec.combined(), elapsed_ms);
    if report.os.is_empty() && report.items.iter().all(|item| !item.found) {
        report.ok = false;
        report.message = crate::i18n::env_no_output(
            crate::i18n::current(),
            &exec.exit_note(),
            exec.combined().trim(),
        );
    } else {
        let found = report.items.iter().filter(|item| item.found).count();
        let lang = crate::i18n::current();
        let extra = if report.xcodes.is_empty() {
            String::new()
        } else {
            crate::i18n::env_xcode_found(lang, report.xcodes.len())
        };
        report.message = crate::i18n::env_ready(lang, found, report.items.len(), &extra);
    }
    report
}

/// Which Xcode to pass as `DEVELOPER_DIR` when building (maps the `.app` path
/// to `Contents/Developer`).
pub fn developer_dir(xcode_app: &str) -> String {
    let trimmed = xcode_app.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with("Contents/Developer") {
        return trimmed.to_string();
    }
    format!("{trimmed}/Contents/Developer")
}

/// Build the `export DEVELOPER_DIR=...` snippet (empty path -> empty string).
pub fn developer_dir_export(xcode_app: &str) -> String {
    let dir = developer_dir(xcode_app);
    if dir.is_empty() {
        String::new()
    } else {
        format!("export DEVELOPER_DIR={}", shell_path(&dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        Client {
            id: "c-1".to_string(),
            name: "mac".to_string(),
            ..Client::default()
        }
    }

    #[test]
    fn script_guards_every_probe() {
        let script = probe_script();
        for probe in PROBES {
            assert!(
                script.contains(&format!("{SECTION_MARK}{}", probe.id)),
                "missing section marker for {}",
                probe.id
            );
            assert!(
                script.contains(&format!("command -v {} ", probe.binary)),
                "{} has no command -v guard",
                probe.id
            );
        }
        // Every guard needs an else branch emitting the missing marker, otherwise a
        // missing tool looks like "found but no output"
        assert_eq!(
            script.matches(MISSING_MARK).count(),
            PROBES.len(),
            "missing-marker count should match the probe count"
        );
        // The Xcode scan must be limited to Darwin
        assert!(script.contains("Darwin"));
        assert!(script.contains("/Applications/Xcode*.app"));
        assert!(script.contains("DEVELOPER_DIR=\"$app/Contents/Developer\""));
        assert!(script.contains("xcodebuild -license status"));
    }

    #[test]
    fn split_sections_keeps_bodies_in_order() {
        let text = format!(
            "{SECTION_MARK}os\nDarwin\n{SECTION_MARK}java\nopenjdk version \"21.0.1\"\n{SECTION_MARK}disk\n/dev/disk1 100 40 60 40% /\n"
        );
        let sections = split_sections(&text);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].0, "os");
        assert_eq!(sections[0].1.trim(), "Darwin");
        assert!(sections[1].1.contains("openjdk"));
        assert!(sections[2].1.contains("/dev/disk1"));
    }

    #[test]
    fn version_extraction_handles_real_tool_output() {
        assert_eq!(extract_version("cargo 1.100.0-nightly (abc 2024-01-01)"), "1.100.0");
        assert_eq!(extract_version("rustc 1.79.0 (129f3b996 2024-06-10)"), "1.79.0");
        assert_eq!(extract_version("Xcode 15.4\nBuild version 15F31d"), "15.4");
        assert_eq!(extract_version("openjdk version \"21.0.1\" 2023-10-17"), "21.0.1");
        assert_eq!(extract_version("Homebrew 4.3.1"), "4.3.1");
        assert_eq!(extract_version("rsync  version 3.2.7  protocol version 31"), "3.2.7");
        assert_eq!(extract_version("1.15.2"), "1.15.2");
        assert_eq!(extract_version("Dart SDK version: 3.4.0 (stable)"), "3.4.0");
        // If nothing can be extracted, return an empty string; do not treat the
        // 3 in "3 files" as a version
        assert_eq!(extract_version("no digits here"), "");
        assert_eq!(extract_version("3 files changed"), "");
        assert_eq!(extract_version(""), "");
    }

    #[test]
    fn parses_xcode_instances_with_their_own_versions() {
        let text = format!(
            "{MARK}\t/Applications/Xcode.app\nXcode 15.4\nBuild version 15F31d\n\
             {MARK}\t/Applications/Xcode-beta.app\nXcode 16.0\nBuild version 16A5171c\n",
            MARK = INSTANCE_MARK
        );
        let list = parse_xcodes(&text);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, "/Applications/Xcode.app");
        assert_eq!(list[0].version, "15.4");
        assert_eq!(list[0].build, "15F31d");
        assert_eq!(
            list[0].developer_dir,
            "/Applications/Xcode.app/Contents/Developer"
        );
        assert_eq!(list[1].version, "16.0");
        // Output without an Xcode must not invent instances
        assert!(parse_xcodes("").is_empty());
        assert!(parse_xcodes("xcode-select: error: tool 'xcodebuild' requires Xcode").is_empty());
    }

    #[test]
    fn disk_parsing_covers_posix_and_darwin_layouts() {
        let disk = parse_disk("/dev/disk3s5 971350180 412345678 558000000 43% /System/Volumes/Data")
            .expect("should parse");
        assert_eq!(disk.available_bytes, 558_000_000 * 1024);
        assert_eq!(disk.mount, "/System/Volumes/Data");
        assert!((disk.used_ratio - 0.4245).abs() < 0.001);

        // macOS mount points can contain spaces
        let disk = parse_disk("/dev/disk1 100 40 60 40% /Volumes/My Disk").unwrap();
        assert_eq!(disk.mount, "/Volumes/My Disk");

        assert!(parse_disk("").is_none());
        assert!(parse_disk("garbage").is_none());
    }

    #[test]
    fn report_marks_missing_tools_and_groups_them() {
        let text = format!(
            "{S}os\nDarwin\n\
             {S}cargo\ncargo 1.100.0 (abc 2024-01-01)\n\
             {S}xcodebuild\n{M}\n\
             {S}java\nopenjdk version \"21.0.1\"\n\
             {S}disk\n/dev/disk1 100 40 60 40% /\n\
             {S}license\nYou have not agreed to the Xcode license agreements.\n\
             {S}xcode\n{INST}\t/Applications/Xcode.app\nXcode 15.4\nBuild version 15F31d\n",
            S = SECTION_MARK,
            M = MISSING_MARK,
            INST = INSTANCE_MARK,
        );
        let report = parse_report(&client(), &text, 42);

        assert!(report.ok);
        assert_eq!(report.os, "Darwin");
        assert_eq!(report.elapsed_ms, 42);
        assert_eq!(report.get("cargo").unwrap().version, "1.100.0");
        assert!(!report.get("xcodebuild").unwrap().found);
        assert_eq!(report.get("xcodebuild").unwrap().version, "");
        assert!(report.get("java").unwrap().found && report.get("java").unwrap().version == "21.0.1");
        assert_eq!(report.xcodes.len(), 1);
        assert!(report.disk.is_some());
        // License not accepted -> license_ok = false
        assert!(!report.license_ok);

        // Grouping: rust and gradle each have hits, common is all missing
        let rust = report.groups.iter().find(|g| g.platform == PLATFORM_RUST).unwrap();
        assert!(rust.available);
        let gradle = report.groups.iter().find(|g| g.platform == PLATFORM_GRADLE).unwrap();
        assert!(gradle.available);
        let common = report
            .groups
            .iter()
            .find(|g| g.platform == PLATFORM_COMMON)
            .unwrap();
        assert!(!common.available);
    }

    #[test]
    fn developer_dir_normalizes() {
        assert_eq!(
            developer_dir("/Applications/Xcode.app"),
            "/Applications/Xcode.app/Contents/Developer"
        );
        assert_eq!(
            developer_dir("/Applications/Xcode.app/"),
            "/Applications/Xcode.app/Contents/Developer"
        );
        // Already Contents/Developer, do not append another layer
        assert_eq!(
            developer_dir("/Applications/Xcode.app/Contents/Developer"),
            "/Applications/Xcode.app/Contents/Developer"
        );
        assert_eq!(developer_dir(""), "");
        assert_eq!(developer_dir("   "), "");
    }

    #[test]
    fn developer_dir_export_is_shell_safe() {
        assert_eq!(
            developer_dir_export("/Applications/Xcode.app"),
            "export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer"
        );
        // Paths with spaces/quotes must be quoted
        let exported = developer_dir_export("/Volumes/My Disk/Xcode.app");
        assert!(exported.contains("'/Volumes/My Disk/Xcode.app/Contents/Developer'"), "{exported}");
        assert_eq!(developer_dir_export(""), "");
    }

    /// Regression fixture: raw output captured from a **real Linux machine**
    /// (see fixtures/README).
    ///
    /// It pins down two things: how the script behaves under a real POSIX shell,
    /// and how the parser handles those real shapes — including the dirty case
    /// where a command exists but no version can be read from its output.
    #[test]
    fn parses_a_captured_real_linux_run() {
        let text = include_str!("../fixtures/probe-linux.txt");
        let report = parse_report(&client(), text, 137);

        assert!(report.ok, "real-machine output must not be judged a failure:{}", report.message);
        assert_eq!(report.os, "Linux");

        // No macOS toolchain on this machine — it must honestly report missing,
        // not invent a version number
        for id in ["xcodebuild", "cocoapods", "homebrew", "java", "gradle", "xcodebuildmcp"] {
            let item = report.get(id).unwrap_or_else(|| panic!("missing entry {id}"));
            assert!(!item.found, "{id} must not be reported as present");
            assert!(item.version.is_empty());
        }

        // Tools that really exist must yield a version
        assert_eq!(report.get("cargo").unwrap().version, "1.97.1");
        assert_eq!(report.get("rustc").unwrap().version, "1.97.1");
        assert_eq!(report.get("rsync").unwrap().version, "3.4.1");

        // Not Darwin: must not invent Xcode instances or a license verdict
        assert!(report.xcodes.is_empty());
        assert!(report.xcode_license.is_empty());
        assert!(!report.license_ok);

        // Disk: the fixture holds `/dev/sdd 1055762868 54894604 947164792 6% /`
        let disk = report.disk.as_ref().expect("should parse a disk");
        assert_eq!(disk.mount, "/");
        assert_eq!(disk.available_bytes, 947_164_792 * 1024);
        assert_eq!(disk.total_bytes, 1_055_762_868 * 1024);
        assert!((disk.used_ratio - 0.051_998).abs() < 0.001);

        // Grouping: rust has hits, xcode is all missing, common has an rsync hit
        let by_platform = |name: &str| {
            report
                .groups
                .iter()
                .find(|group| group.platform == name)
                .unwrap_or_else(|| panic!("missing {name} group"))
        };
        assert!(by_platform(PLATFORM_RUST).available);
        assert!(!by_platform(PLATFORM_XCODE).available);
        assert!(by_platform(PLATFORM_COMMON).available);
        assert_eq!(report.groups.len(), 6, "all six groups must appear (even empty ones, so we know what is missing)");
    }

    /// "Command present but no readable version" must be reported honestly and
    /// must not count as a found version.
    #[test]
    fn present_but_unreadable_version_is_reported_as_found_without_version() {
        let text = format!(
            "{S}os\nLinux\n{S}flutter\n/mnt/d/flutter/bin/internal/shared.sh: line 5: $'\\r': command not found\n",
            S = SECTION_MARK
        );
        let report = parse_report(&client(), &text, 1);
        let flutter = report.get("flutter").unwrap();
        assert!(flutter.found, "any output counts as present");
        assert!(flutter.version.is_empty(), "if no version can be read, do not invent one");
        assert!(report.ok);
    }
}
