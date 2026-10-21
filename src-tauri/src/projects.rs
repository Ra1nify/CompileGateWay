//! Project registry: a source folder on the host plus its remote location on a client.
//!
//! Stored as `projects.json` next to config.json.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "projects.json";

/// Namespace prefix for `{{inputs.<name>}}` (design doc §7.6.4).
///
/// Keeps the existing `{{...}}` syntax and adds a namespace instead of introducing a
/// `$VAR` syntax: two variable syntaxes in one project JSON invite mistakes.
pub const INPUT_NAMESPACE: &str = "inputs.";

/// An **input** a task declares (design doc §7.6.3).
///
/// The value comes from `Task::variables`, else this `default`; `{{inputs.<name>}}`
/// expands in the task's path fields. Values ride on `variables` rather than a new
/// `with` field so `BatchStep` keeps its existing `{ taskId, variables }` shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "TaskInputSpec", extend("required" = ["type"]))]
pub struct InputSpec {
    /// `string` (default) / `enum` / `path` / `bool` / `number`. For validation and UI hints only.
    pub r#type: String,
    /// Default value when the user gives none.
    pub default: Option<String>,
    /// Only meaningful for `path` inputs: which side the path belongs to
    /// (`remote` / `host`). Empty means "do not check the side".
    pub side: Option<crate::paths::Side>,
    /// Allowed values for `enum`; when non-empty the value must be one of them.
    pub values: Vec<String>,
}

/// An **output** a task declares (design doc §7.6.3).
///
/// After a run each one is checked for existence and type (`file` / `directory`).
/// Once outputs are declared, exit code 0 alone is no longer success.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "TaskOutputSpec", extend("required" = ["side", "path"]))]
pub struct OutputSpec {
    /// `file` / `directory`.
    pub r#type: String,
    /// Which side the output is on.
    pub side: crate::paths::Side,
    /// Relative path template (may contain `{{inputs.x}}`), relative to that side's project root.
    pub path: String,
    /// Required (default true). `false` = a missing output is only noted, not a failure.
    pub required: Option<bool>,
}

impl OutputSpec {
    /// Missing `required` means required: declaring an output and ignoring it is pointless.
    pub fn is_required(&self) -> bool {
        self.required.unwrap_or(true)
    }

    /// Expected type; an unrecognized value is treated as `file`.
    pub fn wants_directory(&self) -> bool {
        self.r#type.trim().eq_ignore_ascii_case("directory")
            || self.r#type.trim().eq_ignore_ascii_case("dir")
    }
}

/// Task kinds (design doc §7.1).
pub const KIND_BUILD: &str = "build";
pub const KIND_PUSH: &str = "push";
pub const KIND_PULL: &str = "pull";
pub const KIND_COPY: &str = "copy";
pub const KIND_MOVE: &str = "move";
pub const KIND_DELETE: &str = "delete";
pub const KIND_EXPORT: &str = "export";

pub const KINDS: [&str; 7] = [
    KIND_BUILD,
    KIND_PUSH,
    KIND_PULL,
    KIND_COPY,
    KIND_MOVE,
    KIND_DELETE,
    KIND_EXPORT,
];

/// Fields each task kind **must** provide (`kind` itself is added by the caller).
///
/// The table has one use: generating the `oneOf` branches of the task JSON Schema
/// (`crate::jsonschema`). So "which fields does this kind need" has a single source,
/// and a test checks [`Task::validate()`] against it -- change one side alone and it breaks.
pub fn required_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        KIND_BUILD => &["manifest"],
        // push needs nothing: source dir and remote location both live on the project
        KIND_PUSH => &[],
        KIND_COPY | KIND_MOVE => &["from", "to"],
        KIND_DELETE => &["target"],
        // pull needs a destination; pull and export both need at least one source
        KIND_PULL => &["sources", "to"],
        KIND_EXPORT => &["sources"],
        _ => &[],
    }
}

/// One trigger task in a project (design doc §7.1, §7.2).
///
/// A single struct carries every kind, each using a subset of the fields. `to_json`
/// emits only kind-relevant fields, so the shape written to projects.json matches the
/// design doc examples; missing fields default on deserialize, so hand-edited JSON does
/// not have to be complete.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Task {
    pub id: String,
    /// build | push | pull | copy | move | delete | export
    pub kind: String,
    pub name: String,

    // ---- build ----
    pub manifest: String,
    /// Variable name -> value (BTreeMap keeps the on-disk order stable)
    pub variables: BTreeMap<String, String>,
    /// signed | unsigned (design doc §5)
    pub signing_mode: String,
    /// Single manual artifact path; `artifacts` wins when non-empty
    pub artifact_path: String,
    /// Manual artifact path list (design doc §7.1)
    pub artifacts: Vec<String>,
    /// Xcode used by this build (`.app` path). Empty = do not set `DEVELOPER_DIR`,
    /// i.e. use the target machine's global `xcode-select` choice (design doc §8).
    pub xcode_path: String,

    // ---- push ----
    /// Wipe the remote project directory before pushing (design doc §9 "overwrite")
    pub clean: bool,
    /// Push bytes as-is, **without** CRLF -> LF normalization.
    ///
    /// Default false: a CRLF `gradlew` checked out on Windows trips over the `\r` in its
    /// shebang once pushed to a Mac (`/bin/sh^M: no such file or directory`, acceptance
    /// notes §4.4). Only files with `#!` are touched; binaries stay byte-for-byte identical.
    pub keep_crlf: bool,

    // ---- copy / move ----
    /// Source location.
    pub from: crate::paths::Location,
    /// Destination location. **It is the full path itself** (so it can rename), not a
    /// directory to place into; all directions behave that way
    /// (`fileops::upload_command` handles the cross-side case).
    pub to: crate::paths::Location,
    /// Overwrite when the copy / move / pull destination already exists
    pub overwrite: bool,

    // ---- delete ----
    pub target: crate::paths::Location,

    // ---- pull / export ----
    pub sources: Vec<crate::paths::Location>,
    /// Output file name for export (lands in `<host>/exports/`)
    pub output_name: String,

    // ---- common: input / output contract (design doc §7.6.3)----
    /// Declared inputs (name -> spec). **Values** come from `variables`, else the spec's
    /// `default`; reference one from a field with `{{inputs.<name>}}`.
    pub inputs: BTreeMap<String, InputSpec>,
    /// Declared outputs (name -> spec), checked for existence and type after a run:
    /// once declared, exit code 0 alone is not success.
    pub outputs: BTreeMap<String, OutputSpec>,
}

impl Task {
    pub fn kind(&self) -> &str {
        if self.kind.trim().is_empty() {
            KIND_BUILD
        } else {
            self.kind.trim()
        }
    }

    pub fn is_known_kind(&self) -> bool {
        KINDS.contains(&self.kind())
    }

    /// Display name (falls back to the kind or the manifest id).
    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        if !self.manifest.trim().is_empty() {
            return self.manifest.trim().to_string();
        }
        self.kind().to_string()
    }

    /// Artifact templates this build ends up handling (`artifacts` first, then `artifactPath`).
    pub fn artifact_templates(&self) -> Vec<String> {
        crate::manifest::artifact_templates(&self.artifacts, &self.artifact_path)
    }

    /// Rendered artifact paths; if neither field is set, fall back to the manifest defaults.
    pub fn resolve_artifacts(&self, manifest: &crate::manifest::Manifest) -> Vec<String> {
        crate::build::resolve_artifacts(manifest, &self.variables, &self.artifact_templates())
    }

    /// Environment for this build only (design doc §8: `DEVELOPER_DIR` is passed per run,
    /// never set globally).
    pub fn build_env(&self) -> Vec<(String, String)> {
        let dir = crate::env::developer_dir(&self.xcode_path);
        if dir.is_empty() {
            Vec::new()
        } else {
            vec![("DEVELOPER_DIR".to_string(), dir)]
        }
    }

    /// Check the task for internal consistency; returns human-readable errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.trim().is_empty() {
            errors.push("任务缺少 id".to_string());
        }
        if !self.is_known_kind() {
            errors.push(format!(
                "未知的任务类型 {}:{}(可用:{})",
                self.kind,
                self.title(),
                KINDS.join(" / ")
            ));
            return errors;
        }

        let check_relative = |label: &str, raw: &str| -> Option<String> {
            match crate::paths::normalize_relative(raw) {
                Ok(value) if !value.is_empty() => None,
                Ok(_) => Some(format!("{label} 不能是项目根目录本身")),
                Err(err) => Some(format!("{label} 不合法:{err}")),
            }
        };

        match self.kind() {
            KIND_BUILD => {
                if self.manifest.trim().is_empty() {
                    errors.push("build 任务必须指定 manifest".to_string());
                }
                for template in self.artifact_templates() {
                    if template.contains("..") {
                        errors.push(format!("产物路径里不允许出现 ..:{template}"));
                    }
                }
            }
            KIND_COPY | KIND_MOVE => {
                let mut ok = true;
                for (label, location) in [("from", &self.from), ("to", &self.to)] {
                    if let Some(err) = check_relative(label, &location.path) {
                        errors.push(err);
                        ok = false;
                    }
                }
                // A target inside its own source always fails at run time, so reject it
                // while saving. Only meaningful same-side: cross-side endpoints live on
                // different machines, so nesting cannot happen.
                if ok && self.from.side == self.to.side {
                    let from = crate::paths::normalize_relative(&self.from.path).unwrap_or_default();
                    let to = crate::paths::normalize_relative(&self.to.path).unwrap_or_default();
                    if !from.is_empty() && (to == from || to.starts_with(&format!("{from}/"))) {
                        errors.push(format!(
                            "to({})位于 from({from})之内,会把目录复制进自己里面",
                            self.to.path.trim()
                        ));
                    }
                }
            }
            KIND_DELETE => {
                if let Some(err) = check_relative("target", &self.target.path) {
                    errors.push(err);
                }
            }
            KIND_PULL | KIND_EXPORT => {
                if self.sources.is_empty() {
                    errors.push(format!("{} 任务至少需要一个 sources 条目", self.kind()));
                }
                for (index, source) in self.sources.iter().enumerate() {
                    if let Some(err) = check_relative(&format!("sources[{index}]"), &source.path) {
                        errors.push(err);
                    }
                }
                if self.kind() == KIND_PULL {
                    if let Some(err) = check_relative("to", &self.to.path) {
                        errors.push(err);
                    }
                }
            }
            _ => {}
        }

        // Input/output contract (design doc §7.6.3 / §7.6.4): **the static part is caught
        // while saving**. The expanded result is checked again (values may contain `..`)
        // in `contract::resolved`.
        for (name, spec) in &self.inputs {
            if name.trim().is_empty() {
                errors.push("输入的名字不能为空".to_string());
            }
            let type_code = spec.r#type.trim().to_ascii_lowercase();
            if !matches!(
                type_code.as_str(),
                "" | "string" | "enum" | "path" | "bool" | "number"
            ) {
                errors.push(format!(
                    "输入 {name} 的类型不认识:{}(可用:string / enum / path / bool / number)",
                    spec.r#type.trim()
                ));
            }
            if type_code == "enum" && spec.values.is_empty() {
                errors.push(format!("输入 {name} 声明成 enum,但没有给 values"));
            }
        }
        for name in crate::contract::referenced_inputs(self) {
            if !self.inputs.contains_key(&name) {
                errors.push(format!("{{{{inputs.{name}}}}} 引用了没有声明的输入"));
            }
        }
        for (name, spec) in &self.outputs {
            if name.trim().is_empty() {
                errors.push("输出的名字不能为空".to_string());
            }
            if spec.path.trim().is_empty() {
                errors.push(format!("输出 {name} 缺少 path"));
            }
            if spec.path.contains("..") {
                errors.push(format!(
                    "输出 {name} 的 path 里不允许出现 ..:{}",
                    spec.path
                ));
            }
            let type_code = spec.r#type.trim().to_ascii_lowercase();
            if !matches!(type_code.as_str(), "" | "file" | "directory" | "dir") {
                errors.push(format!(
                    "输出 {name} 的类型只能是 file 或 directory:{}",
                    spec.r#type.trim()
                ));
            }
        }

        errors
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": self.id,
            "kind": self.kind(),
            "name": self.name,
        });
        let map = value.as_object_mut().expect("刚构造的对象");

        match self.kind() {
            KIND_BUILD => {
                map.insert("manifest".to_string(), serde_json::json!(self.manifest));
                map.insert("variables".to_string(), serde_json::json!(self.variables));
                map.insert(
                    "signingMode".to_string(),
                    serde_json::json!(if self.signing_mode.is_empty() {
                        "unsigned"
                    } else {
                        &self.signing_mode
                    }),
                );
                if !self.artifact_path.is_empty() {
                    map.insert(
                        "artifactPath".to_string(),
                        serde_json::json!(self.artifact_path),
                    );
                }
                if !self.artifacts.is_empty() {
                    map.insert("artifacts".to_string(), serde_json::json!(self.artifacts));
                }
                if !self.xcode_path.trim().is_empty() {
                    map.insert(
                        "xcodePath".to_string(),
                        serde_json::json!(self.xcode_path.trim()),
                    );
                }
            }
            KIND_PUSH => {
                map.insert("clean".to_string(), serde_json::json!(self.clean));
            }
            KIND_COPY | KIND_MOVE => {
                map.insert("from".to_string(), self.from.to_json());
                map.insert("to".to_string(), self.to.to_json());
                map.insert("overwrite".to_string(), serde_json::json!(self.overwrite));
            }
            KIND_DELETE => {
                map.insert("target".to_string(), self.target.to_json());
            }
            KIND_PULL => {
                map.insert(
                    "sources".to_string(),
                    serde_json::json!(self
                        .sources
                        .iter()
                        .map(crate::paths::Location::to_json)
                        .collect::<Vec<_>>()),
                );
                map.insert("to".to_string(), self.to.to_json());
                map.insert("overwrite".to_string(), serde_json::json!(self.overwrite));
            }
            KIND_EXPORT => {
                map.insert(
                    "sources".to_string(),
                    serde_json::json!(self
                        .sources
                        .iter()
                        .map(crate::paths::Location::to_json)
                        .collect::<Vec<_>>()),
                );
                map.insert(
                    "outputName".to_string(),
                    serde_json::json!(if self.output_name.trim().is_empty() {
                        "export.zip"
                    } else {
                        &self.output_name
                    }),
                );
            }
            _ => {}
        }

        // Inputs/outputs are **shared by every kind** (design doc §7.6.3), so they go
        // after the match
        if !self.inputs.is_empty() {
            map.insert("inputs".to_string(), serde_json::json!(self.inputs));
        }
        if !self.outputs.is_empty() {
            map.insert("outputs".to_string(), serde_json::json!(self.outputs));
        }

        value
    }
}

/// One step of a batch workflow: references a project task by id (design doc §7.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(extend("required" = ["taskId"]))]
pub struct BatchStep {
    /// Referenced task id. Non-empty: an empty taskId is meaningless and is
    /// **rejected while saving** (it used to be silently dropped).
    #[schemars(length(min = 1))]
    pub task_id: String,
    /// Name of this step, so later steps can reference its outputs (design doc §7.6.5).
    ///
    /// Must be unique within a batch; later steps put `{{steps.<id>.outputs.<name>}}` in
    /// a `variables` value and it becomes the earlier step's **verified** real path.
    /// Empty = this step exposes no outputs.
    pub id: String,
    /// Optional: variables overriding this step alone (for build tasks).
    ///
    /// A value may also hold `{{steps.<id>.outputs.<name>}}` -- that is the channel for
    /// passing data between steps (design doc §7.6.5; no separate `with` field, see §7.6.9).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Batch {
    pub id: String,
    pub name: String,
    pub steps: Vec<BatchStep>,
}

impl Batch {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "steps": self.steps.iter().map(|step| {
                let mut value = serde_json::json!({
                    "taskId": step.task_id,
                    "variables": step.variables,
                });
                if !step.id.trim().is_empty() {
                    value["id"] = serde_json::json!(step.id);
                }
                value
            }).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Id of the client this project belongs to.
    pub client_id: String,
    /// Source directory on the host (the machine running the daemon).
    pub source_path: String,
    /// Location on that client: `<baseDir>/<slug>`.
    pub remote_path: String,
    /// Names excluded on push (directory-name prefix match, or a relative path with `/`).
    pub excludes: Vec<String>,
    /// Registration time (unix millis).
    pub added_at: u64,
    /// Most recent sync.
    pub last_sync_at: u64,
    pub last_sync_ok: bool,
    pub last_sync_message: String,
    /// Trigger tasks on this project.
    pub tasks: Vec<Task>,
    /// Batch workflows.
    pub batches: Vec<Batch>,
    /// Most recent build.
    pub last_build_at: u64,
    pub last_build_ok: bool,
    pub last_build_message: String,
    /// Artifacts located by the last build (possibly several, newline separated).
    pub last_artifact: String,
}

impl Project {
    pub fn task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == task_id)
    }

    /// Match by id first, then by name: hand-written batch names are friendlier in the
    /// CLI than memorizing `b-xxxx`, and the WebUI always has ids, so nothing is ambiguous.
    pub fn batch(&self, batch_id: &str) -> Option<&Batch> {
        self.batches
            .iter()
            .find(|batch| batch.id == batch_id)
            .or_else(|| {
                self.batches
                    .iter()
                    .find(|batch| batch.name == batch_id)
            })
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "clientId": self.client_id,
            "sourcePath": self.source_path,
            "remotePath": self.remote_path,
            "excludes": self.excludes,
            "addedAt": self.added_at,
            "lastSyncAt": self.last_sync_at,
            "lastSyncOk": self.last_sync_ok,
            "lastSyncMessage": self.last_sync_message,
            "tasks": self.tasks.iter().map(Task::to_json).collect::<Vec<_>>(),
            "batches": self.batches.iter().map(Batch::to_json).collect::<Vec<_>>(),
            "lastBuildAt": self.last_build_at,
            "lastBuildOk": self.last_build_ok,
            "lastBuildMessage": self.last_build_message,
            "lastArtifact": self.last_artifact,
        })
    }
}

/// Default push excludes: VCS metadata and OS junk files.
pub const DEFAULT_EXCLUDES: [&str; 3] = [".git", ".DS_Store", "Thumbs.db"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectStore {
    pub projects: Vec<Project>,
}

impl ProjectStore {
    pub fn load(path: &Path) -> ProjectStore {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text + "\n")
    }
}

/// projects.json, in the same directory as config.json.
pub fn store_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FILE_NAME)
}

/// Running job id (the busy error of design doc §7.5 carries it).
pub fn new_job_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 5];
    OsRng.fill_bytes(&mut buf);
    format!("j-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Build task id.
pub fn new_task_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    format!("t-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Batch workflow id.
pub fn new_batch_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    format!("b-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Host-managed directory on the daemon side: `<home>/CompileGateway/<slug>`.
///
/// Relative paths of `side: "host"` file tasks are rooted here (design doc §7.2).
pub fn host_root(project: &Project) -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("CompileGateway").join(slug(&project.name))
}

pub fn new_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    format!("p-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Make a name directory-safe: `Spice Compose` -> `spice-compose`.
/// Remote directories are named from it, so renaming a project leaves the remote dir alone.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if ch.is_alphanumeric() {
            // Non-ASCII alphanumerics (CJK, for example) are kept as-is
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

/// Use the last segment of a host path as the default project name.
pub fn folder_name(path: &str) -> String {
    let cleaned = path.trim_end_matches(['/', '\\']);
    cleaned
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(cleaned)
        .to_string()
}

/// Join a base directory and a slug into a remote path, handling trailing slashes.
pub fn join_remote(base_dir: &str, slug: &str) -> String {
    let base = base_dir.trim().trim_end_matches('/');
    if base.is_empty() {
        slug.to_string()
    } else {
        format!("{base}/{slug}")
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_from(raw: &str) -> Task {
        serde_json::from_str(raw).expect("failed to parse task JSON")
    }

    /// The three examples in design doc §7.2 must work verbatim.
    #[test]
    fn design_doc_task_examples_parse() {
        let build = task_from(
            r#"{
              "id": "ios-release", "name": "iOS Release", "kind": "build",
              "manifest": "kotlin-multiplatform-ios",
              "variables": { "scheme": "iosApp", "configuration": "Release" },
              "artifactPath": ".cgw/derived-data/Build/Products/Release-iphoneos/SpiceCompose.app"
            }"#,
        );
        assert!(build.validate().is_empty(), "{:?}", build.validate());
        assert_eq!(build.artifact_templates().len(), 1);

        let copy = task_from(
            r#"{
              "id": "copy-app-to-staging", "name": "复制到 staging", "kind": "copy",
              "from": { "side": "remote", "path": ".cgw/x.app" },
              "to": { "side": "remote", "path": ".artifacts/staging/x.app" },
              "overwrite": true
            }"#,
        );
        assert!(copy.validate().is_empty(), "{:?}", copy.validate());
        assert!(copy.overwrite);

        let export = task_from(
            r#"{
              "id": "export-latest", "name": "导出最新产物", "kind": "export",
              "sources": [ { "side": "remote", "path": ".artifacts/staging/x.app" } ],
              "outputName": "SpiceCompose-release.zip"
            }"#,
        );
        assert!(export.validate().is_empty(), "{:?}", export.validate());
        assert_eq!(export.sources.len(), 1);
    }

    #[test]
    fn to_json_only_emits_kind_relevant_fields() {
        let copy = Task {
            id: "c1".to_string(),
            kind: KIND_COPY.to_string(),
            name: "copy".to_string(),
            from: crate::paths::Location {
                side: crate::paths::Side::Remote,
                path: "a".to_string(),
            },
            to: crate::paths::Location {
                side: crate::paths::Side::Host,
                path: "b".to_string(),
            },
            overwrite: true,
            ..Task::default()
        };
        let value = copy.to_json();
        assert!(value.get("from").is_some());
        assert!(value.get("to").is_some());
        assert!(value.get("manifest").is_none(), "{value}");
        assert!(value.get("sources").is_none(), "{value}");
        assert!(value.get("outputName").is_none(), "{value}");
    }

    #[test]
    fn task_round_trips_through_json() {
        let original = Task {
            id: "e1".to_string(),
            kind: KIND_EXPORT.to_string(),
            name: "导出".to_string(),
            sources: vec![crate::paths::Location {
                side: crate::paths::Side::Host,
                path: ".artifacts".to_string(),
            }],
            output_name: "out.zip".to_string(),
            ..Task::default()
        };
        let text = serde_json::to_string(&original.to_json()).unwrap();
        let back: Task = serde_json::from_str(&text).unwrap();
        assert_eq!(back.kind(), KIND_EXPORT);
        assert_eq!(back.output_name, "out.zip");
        assert_eq!(back.sources[0].path, ".artifacts");
        assert_eq!(back.sources[0].side, crate::paths::Side::Host);
    }

    #[test]
    fn validation_rejects_escapes_and_unknown_kinds() {
        let bad = task_from(
            r#"{ "id":"x", "kind":"copy", "from":{"side":"remote","path":"../../etc"},
                 "to":{"side":"remote","path":"ok"} }"#,
        );
        let errors = bad.validate();
        assert!(errors.iter().any(|e| e.contains("..")), "{errors:?}");

        let unknown = task_from(r#"{ "id":"x", "kind":"teleport" }"#);
        assert!(unknown
            .validate()
            .iter()
            .any(|e| e.contains("未知的任务类型")));

        let empty_export = task_from(r#"{ "id":"x", "kind":"export", "sources":[] }"#);
        assert!(empty_export
            .validate()
            .iter()
            .any(|e| e.contains("sources")));
    }

    /// A target nested inside its source must be rejected **while saving**, not at run time.
    #[test]
    fn validation_rejects_a_target_inside_its_own_source() {
        let inside = task_from(
            r#"{ "id":"x", "kind":"copy",
                 "from":{"side":"host","path":"dist"},
                 "to":{"side":"host","path":"dist/inner"} }"#,
        );
        let errors = inside.validate();
        assert!(errors.iter().any(|e| e.contains("之内")), "{errors:?}");

        // Identical paths are rejected too
        let same = task_from(
            r#"{ "id":"x", "kind":"move",
                 "from":{"side":"host","path":"a/b"},
                 "to":{"side":"host","path":"a/b"} }"#,
        );
        assert!(same.validate().iter().any(|e| e.contains("之内")), "{:?}", same.validate());

        // Sibling directories are fine
        let sibling = task_from(
            r#"{ "id":"x", "kind":"copy",
                 "from":{"side":"host","path":"dist"},
                 "to":{"side":"host","path":"dist2"} }"#,
        );
        assert!(sibling.validate().is_empty(), "{:?}", sibling.validate());

        // Cross-side endpoints are on different machines, so nesting cannot happen
        let cross = task_from(
            r#"{ "id":"x", "kind":"copy",
                 "from":{"side":"remote","path":"dist"},
                 "to":{"side":"host","path":"dist/inner"} }"#,
        );
        assert!(cross.validate().is_empty(), "{:?}", cross.validate());
    }
    #[test]
    fn legacy_project_json_without_batches_still_loads() {
        // An old projects.json without a batches field must still load
        let store: ProjectStore = serde_json::from_str(
            r#"{"projects":[{"id":"p-1","name":"123","clientId":"c-1",
                "sourcePath":"F:/x","remotePath":"~/CompileGateway/123",
                "tasks":[{"id":"t-1","kind":"build","manifest":"xcode"}]}]}"#,
        )
        .unwrap();
        assert_eq!(store.projects.len(), 1);
        assert!(store.projects[0].batches.is_empty());
        assert_eq!(store.projects[0].tasks[0].kind(), "build");
    }

    #[test]
    fn batch_json_shape_matches_design_doc() {
        let batch = Batch {
            id: "release-pipeline".to_string(),
            name: "Release pipeline".to_string(),
            steps: vec![
                BatchStep {
                    task_id: "ios-release".to_string(),
                    id: String::new(),
                    variables: BTreeMap::new(),
                },
                BatchStep {
                    task_id: "export-latest".to_string(),
                    id: "publish".to_string(),
                    variables: BTreeMap::new(),
                },
            ],
        };
        let value = batch.to_json();
        assert_eq!(value["steps"][0]["taskId"], "ios-release");
        assert_eq!(value["steps"][1]["taskId"], "export-latest");
        // A step without an id must not grow an `id` field; one with an id must carry it
        // (design doc §7.6.5)
        assert!(value["steps"][0].get("id").is_none(), "{value}");
        assert_eq!(value["steps"][1]["id"], "publish");
    }
}
