//! Build-type manifests: `<dir of config.json>/manifest/<id>.json` (design doc §6.2).
//!
//! `include` says what kind of project this is; `build` says how to build it. The file
//! name is the type id.
//!
//! The directory is yours: first launch copies the shipped templates in, and after that
//! the program only writes when you ask it to restore missing templates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::i18n::Lang;

pub const DIR_NAME: &str = "manifest";
/// Legacy directory name. At startup it is renamed in place, so nothing is copied or lost.
pub const LEGACY_DIR_NAME: &str = "variety";
/// Built-in reserved variable: expands to several arguments, so it is not quoted like a
/// normal value (design doc §5).
pub const SIGNING_VAR: &str = "signingExtraArgs";

pub const SIGNING_UNSIGNED: &str =
    "CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=";
pub const SIGNING_SIGNED: &str = "";

// ------------------------------------------------------------------ model

/// Type marker of a manifest (design doc §6.2): any match in `include.paths` claims the
/// project for this type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "ManifestInclude")]
pub struct Include {
    /// Paths relative to the project root, `*` wildcards allowed (e.g. `ios/*.xcodeproj`).
    pub paths: Vec<String>,
}

/// Variable the user fills in before a build (design doc §5); rendered as `{{name}}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "ManifestVariable", extend("required" = ["name"]))]
pub struct Variable {
    /// Name used inside `{{...}}`. Unique per manifest, and not `signingExtraArgs`.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Label shown in the UI; falls back to `name` when empty.
    pub label: String,
    /// Required: without it `build` refuses to run with `variableRequired`.
    pub required: bool,
    /// Default used when the user leaves it empty.
    pub default: Option<String>,
}

/// Step-level output declaration (design doc §7.6.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct StepOutputSpec {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl StepOutputSpec {
    pub fn is_required(&self) -> bool {
        self.required.unwrap_or(true)
    }

    pub fn wants_directory(&self) -> bool {
        self.r#type
            .as_deref()
            .map(|t| t.trim().eq_ignore_ascii_case("directory"))
            .unwrap_or(false)
    }
}

pub fn deserialize_step_outputs<'de, D>(deserializer: D) -> Result<BTreeMap<String, StepOutputSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (key, val) in map {
                match val {
                    serde_json::Value::String(path) => {
                        out.insert(
                            key,
                            StepOutputSpec {
                                path,
                                r#type: None,
                                required: None,
                            },
                        );
                    }
                    serde_json::Value::Object(_) => {
                        if let Ok(spec) = serde_json::from_value::<StepOutputSpec>(val) {
                            out.insert(key, spec);
                        }
                    }
                    _ => {}
                }
            }
            Ok(out)
        }
        serde_json::Value::Array(arr) => {
            let mut out = BTreeMap::new();
            for (index, val) in arr.into_iter().enumerate() {
                if let serde_json::Value::Object(obj) = val {
                    let id = obj
                        .get("id")
                        .or_else(|| obj.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("out{index}"));
                    let path = obj
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let r#type = obj
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let required = obj.get("required").and_then(|v| v.as_bool());
                    out.insert(
                        id,
                        StepOutputSpec {
                            path,
                            r#type,
                            required,
                        },
                    );
                }
            }
            Ok(out)
        }
        _ => Ok(BTreeMap::new()),
    }
}

/// One build step (design doc §6.2 + §7.6.9 item 3): a bare command string, or
/// `{ "id": ..., "run": ..., "outputs": ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "ManifestStep")]
pub enum BuildStep {
    /// Old shape: a bare command.
    Command(#[schemars(length(min = 1))] String),
    /// Named step.
    Named {
        /// Step name. Unique per manifest.
        #[schemars(length(min = 1))]
        id: String,
        /// Command this step runs.
        #[schemars(length(min = 1))]
        run: String,
        /// Step-level outputs (design doc §7.6.5).
        #[serde(
            default,
            skip_serializing_if = "BTreeMap::is_empty",
            deserialize_with = "deserialize_step_outputs"
        )]
        outputs: BTreeMap<String, StepOutputSpec>,
    },
}

impl BuildStep {
    /// Raw command of this step.
    pub fn command(&self) -> &str {
        match self {
            BuildStep::Command(command) => command,
            BuildStep::Named { run, .. } => run,
        }
    }

    /// Step name, or `None` for the old unnamed shape.
    pub fn id(&self) -> Option<&str> {
        match self {
            BuildStep::Command(_) => None,
            BuildStep::Named { id, .. } => Some(id),
        }
    }

    /// Step outputs, or `None` if old command shape or no outputs declared.
    pub fn outputs(&self) -> Option<&BTreeMap<String, StepOutputSpec>> {
        match self {
            BuildStep::Command(_) => None,
            BuildStep::Named { outputs, .. } => {
                if outputs.is_empty() {
                    None
                } else {
                    Some(outputs)
                }
            }
        }
    }
}

/// Build instructions (design doc §6.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "ManifestBuild", extend("required" = ["steps"]))]
pub struct Build {
    /// Shell commands run in order on the target. Non-empty; any non-zero exit stops the run.
    #[schemars(length(min = 1))]
    pub steps: Vec<BuildStep>,
    /// Single default artifact path (design doc §6.2); `artifacts` wins when non-empty.
    pub artifact_path: String,
    /// Artifact list (design doc §7.1): a build may produce several. Wins over `artifactPath`.
    pub artifacts: Vec<String>,
}

/// Directories kept across builds (design doc §6.2), relative to the project root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "ManifestCache")]
pub struct Cache {
    pub paths: Vec<String>,
}

/// A build-type manifest. Only `displayName` and `build` are required; the rest fall back
/// to defaults, but a nameless or stepless manifest is rejected by `Manifest::validate()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(extend("required" = ["displayName", "build"]))]
pub struct Manifest {
    /// Display name (e.g. "Xcode project"). Non-empty.
    #[schemars(length(min = 1))]
    pub display_name: String,
    /// Open string (design doc §6.2): `gradle` / `xcode` / `flutter` / `dart` / `rust` ...
    /// Never validated, so a new platform needs no code change.
    pub platform: String,
    pub include: Include,
    pub variables: Vec<Variable>,
    pub build: Build,
    pub cache: Cache,
}

/// JSON Schema generated from the structs above (design doc §2). See [`crate::jsonschema`]
/// for how and why it is generated; every constraint here is one `Manifest::validate()` enforces.
pub fn json_schema() -> serde_json::Value {
    crate::jsonschema::draft07::<Manifest>(
        "CompileGateway manifest",
        "构建类型定义(设计文档 §6.2)。一份 manifest 同时承担两件事:\
         include 说明\"这是什么类型的工程\",build 说明\"怎么把它构建出来\"。\
         由 Rust 结构体生成,字段名与实现在同一处。",
    )
}

impl Manifest {
    pub fn variable(&self, name: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.name == name)
    }

    /// Validation: unique variable names, no empty steps, every `{{placeholder}}` declared.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.display_name.trim().is_empty() {
            errors.push("displayName 不能为空".to_string());
        }
        if self.build.steps.is_empty() {
            errors.push("build.steps 不能为空".to_string());
        }
        // Steps: commands non-empty, and named steps unique (logs locate failures by name)
        let mut step_ids: Vec<&str> = Vec::new();
        for (index, step) in self.build.steps.iter().enumerate() {
            if step.command().trim().is_empty() {
                errors.push(format!("build.steps 第 {} 步的命令是空的", index + 1));
            }
            if let Some(id) = step.id() {
                let id = id.trim();
                if id.is_empty() {
                    errors.push(format!("build.steps 第 {} 步的 id 是空的", index + 1));
                } else if step_ids.contains(&id) {
                    errors.push(format!("build.steps 的步骤 id 重复:{id}"));
                } else {
                    step_ids.push(id);
                }
            }
            if let Some(outputs) = step.outputs() {
                for (name, spec) in outputs {
                    let name_trimmed = name.trim();
                    if name_trimmed.is_empty() {
                        errors.push(format!("build.steps 第 {} 步有空的输出名", index + 1));
                    }
                    if spec.path.trim().is_empty() {
                        errors.push(format!(
                            "build.steps 第 {} 步输出 {} 的路径是空的",
                            index + 1,
                            name
                        ));
                    }
                    if spec.path.contains("..") {
                        errors.push(format!(
                            "build.steps 第 {} 步输出 {} 的路径不允许包含 ..:{}",
                            index + 1,
                            name,
                            spec.path
                        ));
                    }
                }
            }
        }

        let mut seen = BTreeMap::new();
        for variable in &self.variables {
            let name = variable.name.trim();
            if name.is_empty() {
                errors.push("variables 里有空的 name".to_string());
                continue;
            }
            if name == SIGNING_VAR {
                errors.push(format!("{SIGNING_VAR} 是内置保留变量,不要在 variables 里声明"));
            }
            if seen.insert(name.to_string(), true).is_some() {
                errors.push(format!("变量名重复:{name}"));
            }
        }

        let mut targets: Vec<String> = self
            .build
            .steps
            .iter()
            .map(|step| step.command().to_string())
            .collect();
        if !self.build.artifact_path.is_empty() {
            targets.push(self.build.artifact_path.clone());
        }
        targets.extend(self.build.artifacts.iter().cloned());
        for step in &self.build.steps {
            if let Some(outputs) = step.outputs() {
                for spec in outputs.values() {
                    targets.push(spec.path.clone());
                }
            }
        }
        for text in &targets {
            for placeholder in placeholders(text) {
                if placeholder == SIGNING_VAR {
                    continue;
                }
                if !self.variables.iter().any(|v| v.name == placeholder) {
                    errors.push(format!("占位符 {{{{{placeholder}}}}} 没有在 variables 里声明"));
                }
            }
        }

        errors.sort();
        errors.dedup();
        errors
    }

    pub fn to_json(&self, id: &str, template: bool) -> serde_json::Value {
        let errors = self.validate();
        serde_json::json!({
            "id": id,
            "displayName": self.display_name,
            "platform": self.platform,
            "include": { "paths": self.include.paths },
            "variables": self.variables.iter().map(|v| serde_json::json!({
                "name": v.name,
                "label": if v.label.is_empty() { v.name.clone() } else { v.label.clone() },
                "required": v.required,
                "default": v.default,
            })).collect::<Vec<_>>(),
            "build": {
                "stepCount": self.build.steps.len(),
                "artifactPath": self.build.artifact_path,
                "artifacts": self.build.artifacts,
            },
            "cache": { "paths": self.cache.paths },
            // Marks a shipped template (so it can be restored), not special treatment
            "template": template,
            "source": if template { "shipped" } else { "yours" },
            "valid": errors.is_empty(),
            "errors": errors,
        })
    }
}

/// Names inside `{{name}}` occurrences.
fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'{' && bytes[index + 1] == b'{' {
            if let Some(end) = text[index + 2..].find("}}") {
                let name = text[index + 2..index + 2 + end].trim().to_string();
                if !name.is_empty() {
                    out.push(name);
                }
                index += 2 + end + 2;
                continue;
            }
        }
        index += 1;
    }
    out
}

// ------------------------------------------------------------------ rendering

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningMode {
    Signed,
    Unsigned,
}

impl SigningMode {
    pub fn parse(value: &str) -> SigningMode {
        match value.trim().to_ascii_lowercase().as_str() {
            "signed" => SigningMode::Signed,
            _ => SigningMode::Unsigned,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            SigningMode::Signed => "signed",
            SigningMode::Unsigned => "unsigned",
        }
    }

    fn extra_args(self) -> &'static str {
        match self {
            SigningMode::Signed => SIGNING_SIGNED,
            SigningMode::Unsigned => SIGNING_UNSIGNED,
        }
    }
}

/// Render a manifest into one command: `cd <workdir> && export ... && step1 && step2 && ...`
///
/// `env` (design doc §8) is exported up front so every step sees it; only
/// `{{signingExtraArgs}}` stays unquoted, because it expands to several bare arguments.
pub fn render_command(
    manifest: &Manifest,
    variables: &BTreeMap<String, String>,
    signing: SigningMode,
    workdir: &str,
    env: &[(String, String)],
) -> Result<String, Vec<String>> {
    let mut errors = Vec::new();

    // Resolve each variable to its final value first
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    for variable in &manifest.variables {
        let provided = variables
            .get(&variable.name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let value = provided
            .or_else(|| variable.default.clone())
            .unwrap_or_default();
        if variable.required && value.trim().is_empty() {
            errors.push(variable.name.clone());
        }
        resolved.insert(variable.name.clone(), value);
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let substitute = |text: &str| -> String {
        let mut out = String::new();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if index + 1 < bytes.len() && bytes[index] == b'{' && bytes[index + 1] == b'{' {
                if let Some(end) = text[index + 2..].find("}}") {
                    let name = text[index + 2..index + 2 + end].trim();
                    if name == SIGNING_VAR {
                        out.push_str(signing.extra_args());
                    } else if let Some(value) = resolved.get(name) {
                        out.push_str(&shell_quote(value));
                    } else {
                        // Undeclared placeholders stay verbatim; validate() already reported them
                        out.push_str(&text[index..index + 2 + end + 2]);
                    }
                    index += 2 + end + 2;
                    continue;
                }
            }
            let ch = text[index..].chars().next().unwrap();
            out.push(ch);
            index += ch.len_utf8();
        }
        out
    };

    let steps: Vec<String> = manifest
        .build
        .steps
        .iter()
        .map(|step| substitute(step.command()))
        .collect();
    let joined = steps.join(" && ");

    let mut parts: Vec<String> = Vec::new();
    if !workdir.trim().is_empty() {
        parts.push(format!("cd {}", crate::ssh::shell_path(workdir)));
    }
    for (name, value) in env {
        if value.trim().is_empty() {
            continue;
        }
        // Names are hardcoded; values come from config or detection, so quote them as single values
        parts.push(format!("export {name}={}", shell_quote(value)));
    }
    parts.push(joined);
    Ok(parts.join(" && "))
}

/// Render one path template: variables substituted, but no shell quoting — it is a path.
pub fn render_template(
    manifest: &Manifest,
    variables: &BTreeMap<String, String>,
    template: &str,
) -> String {
    if template.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if index + 1 < bytes.len() && bytes[index] == b'{' && bytes[index + 1] == b'{' {
            if let Some(end) = template[index + 2..].find("}}") {
                let name = template[index + 2..index + 2 + end].trim();
                let value = variables
                    .get(name)
                    .cloned()
                    .or_else(|| manifest.variable(name).and_then(|v| v.default.clone()))
                    .unwrap_or_default();
                out.push_str(value.trim());
                index += 2 + end + 2;
                continue;
            }
        }
        let ch = template[index..].chars().next().unwrap();
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Render `artifactPath`.
pub fn render_artifact(manifest: &Manifest, variables: &BTreeMap<String, String>) -> String {
    render_template(manifest, variables, &manifest.build.artifact_path)
}

/// Render the manifest's own artifact list, falling back to the singular `artifactPath`.
pub fn render_artifacts(manifest: &Manifest, variables: &BTreeMap<String, String>) -> Vec<String> {
    let templates = artifact_templates(&manifest.build.artifacts, &manifest.build.artifact_path);
    templates
        .into_iter()
        .map(|template| render_template(manifest, variables, &template))
        .filter(|path| !path.trim().is_empty())
        .collect()
}

/// Artifact rule: use `artifacts` when non-empty, otherwise the singular `artifactPath`.
/// Task-level overrides live in [`crate::projects::Task::resolve_artifacts`].
pub fn artifact_templates(artifacts: &[String], artifact_path: &str) -> Vec<String> {
    let list: Vec<String> = artifacts
        .iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();
    if !list.is_empty() {
        return list;
    }
    let single = artifact_path.trim();
    if single.is_empty() {
        Vec::new()
    } else {
        vec![single.to_string()]
    }
}

/// Shell-quote one value: safe characters pass through, everything else is single-quoted.
pub fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | ':' | '=' | ',')
        });
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

// ------------------------------------------------------------------ disk

pub fn dir(config_path: &Path) -> PathBuf {
    beside(config_path, DIR_NAME)
}

/// Legacy directory (`variety/`).
pub fn legacy_dir(config_path: &Path) -> PathBuf {
    beside(config_path, LEGACY_DIR_NAME)
}

fn beside(config_path: &Path, name: &str) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name)
}

/// Result of the first-launch setup, reported in the startup log.
pub struct Prepared {
    pub dir: PathBuf,
    /// Whether the directory was created this time.
    pub created: bool,
    /// Whether it was renamed over from the legacy `variety/`.
    pub migrated: bool,
    /// Template ids copied in this time.
    pub seeded: Vec<String>,
}

/// Ensure the build-type directory exists.
///
/// First run creates it and copies the shipped templates in; an existing directory is left
/// alone, so nothing is silently restored. A legacy `variety/` is renamed in place.
pub fn prepare_dir(config_path: &Path) -> Prepared {
    let dir = self::dir(config_path);
    let legacy = legacy_dir(config_path);
    let mut migrated = false;

    if !dir.exists() && legacy.is_dir() {
        if std::fs::rename(&legacy, &dir).is_ok() {
            migrated = true;
        }
    }

    if dir.is_dir() {
        return Prepared {
            dir,
            created: false,
            migrated,
            seeded: Vec::new(),
        };
    }

    let mut seeded = Vec::new();
    for (id, body) in TEMPLATES {
        if write_raw(config_path, id, body).is_ok() {
            seeded.push(id.to_string());
        }
    }
    Prepared {
        dir,
        created: true,
        migrated,
        seeded,
    }
}

/// Build-type templates shipped with the program, as `(id, body)`.
///
/// They are used for exactly two things: seeding `manifest/` on first run, and writing
/// files back when you explicitly restore missing templates. Runtime always reads the
/// file on disk.
pub const TEMPLATES: [(&str, &str); 8] = [
    ("xcode", include_str!("../templates/xcode.json")),
    ("xcode-ipa", include_str!("../templates/xcode-ipa.json")),
    ("rust-native", include_str!("../templates/rust-native.json")),
    ("flutter-ios", include_str!("../templates/flutter-ios.json")),
    (
        "flutter-android",
        include_str!("../templates/flutter-android.json"),
    ),
    (
        "kotlin-multiplatform-ios",
        include_str!("../templates/kotlin-multiplatform-ios.json"),
    ),
    ("gradle-android", include_str!("../templates/gradle-android.json")),
    ("cmake-native", include_str!("../templates/cmake-native.json")),
];

/// Whether this file is one of the shipped templates.
///
/// Not special treatment: it only answers "can this be restored" and "is this still the
/// unmodified template".
pub fn is_template(id: &str) -> bool {
    TEMPLATES.iter().any(|(name, _)| *name == id)
}

pub fn template(id: &str) -> Option<&'static str> {
    TEMPLATES
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, body)| *body)
}

/// Manifest ids currently on disk, sorted.
pub fn list_ids(config_path: &Path) -> Vec<String> {
    let dir = dir(config_path);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                path.file_stem().map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    ids.sort();
    ids
}

pub fn path_of(config_path: &Path, id: &str) -> PathBuf {
    dir(config_path).join(format!("{id}.json"))
}

pub fn load(config_path: &Path, id: &str) -> Result<Manifest, String> {
    let path = path_of(config_path, id);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&raw)
}

pub fn parse(raw: &str) -> Result<Manifest, String> {
    let manifest: Manifest =
        serde_json::from_str(raw).map_err(|e| format!("不是合法 JSON:{e}"))?;
    let errors = manifest.validate();
    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors.join("; "))
    }
}

pub fn parse_json(raw: &str) -> (Option<Manifest>, Vec<String>) {
    match serde_json::from_str::<Manifest>(raw) {
        Ok(manifest) => {
            let errors = manifest.validate();
            (Some(manifest), errors)
        }
        Err(err) => (None, vec![format!("不是合法 JSON:{err}")]),
    }
}

/// Write back to disk, creating the directory if needed.
pub fn write_raw(config_path: &Path, id: &str, raw: &str) -> Result<(), String> {
    let dir = dir(config_path);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let text = if raw.trim().is_empty() {
        "{}".to_string()
    } else {
        // Pretty-print when parseable, so the file is readable directly in the directory
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
            Err(_) => raw.to_string(),
        }
    };
    std::fs::write(path_of(config_path, id), text + "\n")
        .map_err(|e| format!("写入失败:{e}"))
}

pub fn remove(config_path: &Path, id: &str) -> Result<(), String> {
    let path = path_of(config_path, id);
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Seed the templates on first run (only when the directory does not exist at all).
pub fn seed_templates(config_path: &Path) -> Prepared {
    prepare_dir(config_path)
}

/// Which templates are still missing (for "restore missing templates").
pub fn missing_templates(config_path: &Path) -> Vec<(&'static str, String)> {
    TEMPLATES
        .iter()
        .filter(|(id, _)| !path_of(config_path, id).exists())
        .map(|(id, body)| {
            let name = serde_json::from_str::<Manifest>(body)
                .map(|m| m.display_name)
                .unwrap_or_else(|_| (*id).to_string());
            (*id, name)
        })
        .collect()
}

pub fn restore_templates(config_path: &Path, ids: &[String]) -> Vec<String> {
    let mut restored = Vec::new();
    for id in ids {
        if let Some(body) = template(id) {
            if write_raw(config_path, id, body).is_ok() {
                restored.push(id.clone());
            }
        }
    }
    restored
}

// ------------------------------------------------------------------ refresh (snapshot + diff)

/// "Fingerprint" of one manifest file, used by refresh to tell whether the content changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub modified_ms: u64,
    pub size: u64,
}

/// Fingerprint of every manifest in the directory.
pub type Snapshot = BTreeMap<String, Fingerprint>;

/// What is on disk right now; an empty map when the directory is missing.
pub fn snapshot(config_path: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    for id in list_ids(config_path) {
        let Ok(meta) = std::fs::metadata(path_of(config_path, &id)) else {
            continue;
        };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.insert(
            id,
            Fingerprint {
                modified_ms,
                size: meta.len(),
            },
        );
    }
    out
}

/// Difference between two snapshots.
#[derive(Debug, Default)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// One-line human summary (used by the API response and logs).
    ///
    /// Takes a [`Lang`]: this string is rendered to the user (the WebUI's refresh toast
    /// and the CLI), so it follows the configured language. The structured
    /// `added` / `changed` / `removed` arrays remain the machine-readable form.
    pub fn describe(&self, lang: Lang) -> String {
        crate::i18n::manifest_diff_summary(lang, &self.added, &self.changed, &self.removed)
    }
}

pub fn diff(before: &Snapshot, after: &Snapshot) -> Diff {
    let mut out = Diff::default();
    for (id, now) in after {
        match before.get(id) {
            None => out.added.push(id.clone()),
            Some(old) if old != now => out.changed.push(id.clone()),
            Some(_) => {}
        }
    }
    for id in before.keys() {
        if !after.contains_key(id) {
            out.removed.push(id.clone());
        }
    }
    out
}

// ------------------------------------------------------------------ type detection

/// Which build types a project directory matches (design doc §6.2: all `include.paths` must hit).
pub fn detect(source: &Path, manifests: &[(String, Manifest)]) -> Vec<serde_json::Value> {
    let mut matches = Vec::new();
    for (id, manifest) in manifests {
        if manifest.include.paths.is_empty() {
            continue;
        }
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        for pattern in &manifest.include.paths {
            if include_matches(source, pattern) {
                matched.push(pattern.clone());
            } else {
                missing.push(pattern.clone());
            }
        }
        if missing.is_empty() {
            matches.push(serde_json::json!({
                "id": id,
                "displayName": manifest.display_name,
                "matched": matched,
                "missing": missing,
            }));
        }
    }
    matches
}

/// Supports a single `*` in the last segment (`*.xcodeproj`); everything else is an
/// existence check on the joined path.
fn include_matches(source: &Path, pattern: &str) -> bool {
    let pattern = pattern.trim().trim_start_matches("./");
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('*') {
        return source.join(pattern).exists();
    }

    let path = Path::new(pattern);
    let file = path
        .file_name()
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let dir = source.join(parent);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        wildcard_match(&file, &entry.file_name().to_string_lossy())
    })
}

/// Wildcard match supporting only `*` (any length, may appear several times).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    if let Some(first) = parts.first() {
        if !rest.starts_with(first) {
            return false;
        }
        rest = &rest[first.len()..];
    }
    for (index, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            continue;
        }
        if index == parts.len() - 1 {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        serde_json::from_str(
            r#"{
              "displayName": "T",
              "platform": "xcode",
              "include": { "paths": ["a.xcodeproj"] },
              "variables": [
                { "name": "scheme", "label": "S", "required": true },
                { "name": "configuration", "label": "C", "default": "Release" }
              ],
              "build": { "steps": ["build {{scheme}} {{configuration}} {{signingExtraArgs}}"], "artifactPath": "out/{{scheme}}.app" }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn render_escapes_values_but_not_signing_args() {
        let manifest = sample();
        let mut vars = BTreeMap::new();
        vars.insert("scheme".to_string(), "My App; rm -rf /".to_string());
        let command = render_command(&manifest, &vars, SigningMode::Unsigned, "~/work", &[]).unwrap();

        assert!(command.starts_with("cd ~/work && "));
        // Dangerous characters are single-quoted, so this cannot become a second command
        assert!(command.contains("'My App; rm -rf /'"), "{command}");
        // signingExtraArgs expands to several bare arguments
        assert!(command.contains("CODE_SIGNING_ALLOWED=NO"), "{command}");
        assert!(!command.contains("'CODE_SIGNING_ALLOWED=NO"), "{command}");
    }

    #[test]
    fn render_reports_missing_required_variables() {
        let manifest = sample();
        let vars = BTreeMap::new();
        let err = render_command(&manifest, &vars, SigningMode::Signed, "w", &[]).unwrap_err();
        assert_eq!(err, vec!["scheme".to_string()]);
    }

    #[test]
    fn defaults_fill_in() {
        let manifest = sample();
        let mut vars = BTreeMap::new();
        vars.insert("scheme".to_string(), "Runner".to_string());
        let command = render_command(&manifest, &vars, SigningMode::Signed, "w", &[]).unwrap();
        assert!(command.contains("Runner Release"), "{command}");
        // In signed mode signingExtraArgs expands to nothing
        assert!(!command.contains("CODE_SIGNING"), "{command}");
        assert_eq!(render_artifact(&manifest, &vars), "out/Runner.app");
    }

    #[test]
    fn env_vars_are_exported_for_every_step() {
        let manifest = sample();
        let mut vars = BTreeMap::new();
        vars.insert("scheme".to_string(), "Runner".to_string());

        // Design doc §8: pass DEVELOPER_DIR per build instead of changing global xcode-select
        let env = vec![(
            "DEVELOPER_DIR".to_string(),
            "/Applications/Xcode.app/Contents/Developer".to_string(),
        )];
        let command =
            render_command(&manifest, &vars, SigningMode::Signed, "~/work", &env).unwrap();
        // Trailing space: `{{signingExtraArgs}}` expands to an empty string in signed mode
        // and the placeholder leaves a space behind. Harmless to the shell, asserted as-is.
        assert_eq!(
            command,
            "cd ~/work && export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer && build Runner Release "
        );

        // Paths with spaces are quoted too
        let env = vec![(
            "DEVELOPER_DIR".to_string(),
            "/Volumes/My Disk/Xcode.app/Contents/Developer".to_string(),
        )];
        let command = render_command(&manifest, &vars, SigningMode::Signed, "w", &env).unwrap();
        assert!(
            command
                .contains("export DEVELOPER_DIR='/Volumes/My Disk/Xcode.app/Contents/Developer'"),
            "{command}"
        );

        // Empty values are skipped, so no bare `export X=` is emitted
        let env = vec![("DEVELOPER_DIR".to_string(), String::new())];
        let command = render_command(&manifest, &vars, SigningMode::Signed, "w", &env).unwrap();
        assert_eq!(command, "cd w && build Runner Release ");

        // No workdir means no empty `cd` either
        let command = render_command(&manifest, &vars, SigningMode::Signed, "", &env).unwrap();
        assert_eq!(command, "build Runner Release ");
    }

    #[test]
    fn validate_catches_undeclared_placeholders() {        let manifest: Manifest = serde_json::from_str(
            r#"{ "displayName": "x", "build": { "steps": ["echo {{nope}}"] } }"#,
        )
        .unwrap();
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.contains("nope")), "{errors:?}");
    }

    #[test]
    fn shipped_templates_are_valid() {
        for (id, body) in TEMPLATES {
            let manifest: Manifest = serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("template {id} is not valid JSON: {e}"));
            let errors = manifest.validate();
            assert!(errors.is_empty(), "template {id} fails validation: {errors:?}");
            assert!(
                !manifest.build.steps.is_empty(),
                "template {id} has no build.steps"
            );
            assert!(!manifest.include.paths.is_empty(), "template {id} has no include paths");
        }
    }

    /// First run: the directory is created and templates are copied in; the second call
    /// does nothing.
    #[test]
    fn prepare_dir_seeds_once_and_then_leaves_you_alone() {
        let root = std::env::temp_dir().join(format!("cgw-manifest-{}", std::process::id()));
        let config = root.join("config.json");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let first = prepare_dir(&config);
        assert!(first.created, "the directory should be created when missing");
        assert_eq!(first.seeded.len(), TEMPLATES.len());
        assert!(dir(&config).is_dir());
        for (id, _) in TEMPLATES {
            assert!(path_of(&config, id).exists(), "{id} was not copied in");
        }

        // Edit one file, delete another: the second prepare must not interfere
        std::fs::write(path_of(&config, "xcode"), "{ \"mine\": true }").unwrap();
        std::fs::remove_file(path_of(&config, "gradle-android")).unwrap();
        let second = prepare_dir(&config);
        assert!(!second.created);
        assert!(second.seeded.is_empty());
        assert_eq!(
            std::fs::read_to_string(path_of(&config, "xcode")).unwrap(),
            "{ \"mine\": true }"
        );
        assert!(!path_of(&config, "gradle-android").exists(), "do not restore it behind our back");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The legacy `variety/` is renamed in place, contents untouched.
    #[test]
    fn legacy_variety_dir_is_migrated_in_place() {
        let root = std::env::temp_dir().join(format!("cgw-variety-{}", std::process::id()));
        let config = root.join("config.json");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(legacy_dir(&config)).unwrap();
        std::fs::write(legacy_dir(&config).join("mine.json"), "{ \"custom\": 1 }").unwrap();

        let prepared = prepare_dir(&config);
        assert!(prepared.migrated, "the migration should be reported");
        assert!(!prepared.created, "migrating is not creating");
        assert!(!legacy_dir(&config).exists(), "the legacy directory should be gone");
        assert_eq!(
            std::fs::read_to_string(path_of(&config, "mine")).unwrap(),
            "{ \"custom\": 1 }"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Refresh relies on fingerprints: added, changed and removed must each be recognised.
    #[test]
    fn snapshot_diff_reports_add_remove_change() {
        let root = std::env::temp_dir().join(format!("cgw-diff-{}", std::process::id()));
        let config = root.join("config.json");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        prepare_dir(&config);

        let before = snapshot(&config);
        assert_eq!(before.len(), TEMPLATES.len());

        std::fs::write(path_of(&config, "brand-new"), "{ \"a\": 1 }").unwrap();
        std::fs::write(path_of(&config, "xcode"), "{ \"changed\": true }").unwrap();
        std::fs::remove_file(path_of(&config, "flutter-ios")).unwrap();

        let after = snapshot(&config);
        let diff = diff(&before, &after);
        assert_eq!(diff.added, vec!["brand-new".to_string()]);
        assert_eq!(diff.removed, vec!["flutter-ios".to_string()]);
        assert_eq!(diff.changed, vec!["xcode".to_string()]);
        assert!(diff.describe(Lang::ZhHans).contains("brand-new"));
        assert_eq!(diff.describe(Lang::EnUS), "added brand-new;changed xcode;removed flutter-ios");
        assert_eq!(diff.describe(Lang::ZhHans), "新增 brand-new;内容变了 xcode;已删除 flutter-ios");

        // Nothing touched -> no changes
        assert!(super::diff(&after, &snapshot(&config)).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn artifact_list_falls_back_to_single_path() {
        assert_eq!(
            artifact_templates(&["a".to_string()], "ignored"),
            vec!["a".to_string()]
        );
        // Empty artifacts fall back to the singular path
        assert_eq!(artifact_templates(&[], "out.app"), vec!["out.app".to_string()]);
        // Both empty -> empty list
        assert!(artifact_templates(&[], "").is_empty());
        // Blank entries are dropped
        assert_eq!(
            artifact_templates(&[" ".to_string(), "x".to_string()], ""),
            vec!["x".to_string()]
        );
    }

    #[test]
    fn artifact_list_is_rendered_with_variables() {
        let mut manifest = sample();
        manifest.build.artifacts = vec![
            "out/{{scheme}}.app".to_string(),
            "out/{{scheme}}.app.dSYM".to_string(),
        ];
        let mut vars = BTreeMap::new();
        vars.insert("scheme".to_string(), "Runner".to_string());

        assert_eq!(
            render_artifacts(&manifest, &vars),
            vec![
                "out/Runner.app".to_string(),
                "out/Runner.app.dSYM".to_string()
            ]
        );
    }

    #[test]
    fn validate_covers_artifact_list_placeholders() {
        let manifest: Manifest = serde_json::from_str(
            r#"{ "displayName": "x", "build": { "steps": ["echo hi"], "artifacts": ["out/{{typo}}.app"] } }"#,
        )
        .unwrap();
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.contains("typo")), "{errors:?}");
    }

    #[test]
    fn wildcard_matching() {        assert!(wildcard_match("*.xcodeproj", "Runner.xcodeproj"));
        assert!(!wildcard_match("*.xcodeproj", "Runner.xcworkspace"));
        assert!(wildcard_match("*Runner*", "aRunnerb"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exact2"));
    }

    // ------------------------------------------------------ generated JSON Schema

    /// Recursively collect every `properties` name in a schema.
    fn collect_property_names(node: &serde_json::Value, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if key == "properties" {
                        if let Some(props) = value.as_object() {
                            out.extend(props.keys().cloned());
                        }
                    }
                    collect_property_names(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_property_names(item, out);
                }
            }
            _ => {}
        }
    }

    /// Recursively collect every key of an **instance** JSON document.
    fn collect_keys(node: &serde_json::Value, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    out.push(key.clone());
                    collect_keys(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_keys(item, out);
                }
            }
            _ => {}
        }
    }

    /// Both build-step shapes (design doc §7.6.9 item 3): the old string and the new
    /// `{id, run}`. Ids must be unique and commands non-empty; both only affect diagnostics.
    #[test]
    fn build_steps_accept_names_and_keep_the_old_shape() {
        let mixed = r#"{
            "displayName": "T",
            "build": { "steps": [
                "echo one",
                { "id": "link", "run": "echo two" },
                { "id": "package", "run": "echo three" }
            ] }
        }"#;
        let manifest = parse(mixed).unwrap();
        assert_eq!(manifest.build.steps.len(), 3);
        assert_eq!(manifest.build.steps[0].command(), "echo one");
        assert_eq!(manifest.build.steps[0].id(), None);
        assert_eq!(manifest.build.steps[1].id(), Some("link"));
        assert_eq!(manifest.build.steps[1].command(), "echo two");
        // The rendered command still joins the steps in order, exactly as before
        let command = render_command(
            &manifest,
            &BTreeMap::new(),
            SigningMode::Unsigned,
            "",
            &[],
        )
        .unwrap();
        assert!(command.ends_with("echo one && echo two && echo three"), "{command}");

        // Duplicate step ids -> rejected
        let dup = r#"{ "displayName": "T", "build": { "steps": [
            { "id": "a", "run": "echo 1" }, { "id": "a", "run": "echo 2" }] } }"#;
        let errors = parse(dup).unwrap_err();
        assert!(errors.contains("步骤 id 重复"), "{errors}");

        // Empty command -> rejected
        let empty = r#"{ "displayName": "T", "build": { "steps": ["  "] } }"#;
        assert!(parse(empty).unwrap_err().contains("命令是空的"));

        // Steps are an anyOf (string or named object); `items` is a `$ref`, the real
        // content lives in definitions.ManifestStep.
        let schema = json_schema();
        let step_def = schema
            .get("definitions")
            .and_then(|definitions| definitions.get("ManifestStep"))
            .cloned()
            .unwrap_or_default();
        assert!(
            step_def.get("anyOf").is_some(),
            "the step schema should be an anyOf: {step_def}"
        );
        assert_eq!(
            step_def["anyOf"].as_array().map(Vec::len),
            Some(2),
            "two shapes: {step_def}"
        );
    }

    #[test]
    fn build_steps_accept_step_level_outputs() {
        let raw = r#"{
            "displayName": "T",
            "variables": [
                { "name": "arch", "default": "x86_64" }
            ],
            "build": { "steps": [
                {
                    "id": "compile",
                    "run": "make",
                    "outputs": {
                        "bin": "build/{{arch}}/app",
                        "doc": { "path": "docs", "type": "directory", "required": false }
                    }
                },
                {
                    "id": "bundle",
                    "run": "tar czf app.tar.gz build",
                    "outputs": [
                        { "id": "archive", "path": "app.tar.gz" }
                    ]
                }
            ] }
        }"#;
        let manifest = parse(raw).unwrap();
        assert_eq!(manifest.build.steps.len(), 2);

        let step1 = &manifest.build.steps[0];
        let outputs1 = step1.outputs().expect("step 1 should declare outputs");
        assert_eq!(outputs1.len(), 2);
        assert_eq!(outputs1["bin"].path, "build/{{arch}}/app");
        assert_eq!(outputs1["bin"].is_required(), true);
        assert_eq!(outputs1["bin"].wants_directory(), false);
        assert_eq!(outputs1["doc"].path, "docs");
        assert_eq!(outputs1["doc"].is_required(), false);
        assert_eq!(outputs1["doc"].wants_directory(), true);

        let step2 = &manifest.build.steps[1];
        let outputs2 = step2.outputs().expect("step 2 should declare outputs");
        assert_eq!(outputs2["archive"].path, "app.tar.gz");

        // Validate catches undeclared placeholder in step output path
        let bad_ph = r#"{
            "displayName": "T",
            "build": { "steps": [
                { "id": "c", "run": "echo 1", "outputs": { "x": "out/{{unknown}}" } }
            ] }
        }"#;
        assert!(parse(bad_ph).unwrap_err().contains("unknown"));

        // Validate catches .. in step output path
        let bad_escape = r#"{
            "displayName": "T",
            "build": { "steps": [
                { "id": "c", "run": "echo 1", "outputs": { "x": "../escape" } }
            ] }
        }"#;
        assert!(parse(bad_escape).unwrap_err().contains(".."));
    }

    /// Anti-drift: every key of a fully populated manifest must appear in the schema's
    /// `properties`. The schema is generated from these structs, so a hand-written schema
    /// swapped back in would break this test immediately.
    #[test]
    fn generated_schema_covers_every_serialized_field() {
        let schema = json_schema();
        let mut properties = Vec::new();
        collect_property_names(&schema, &mut properties);

        let full = serde_json::json!({
            "displayName": "T",
            "platform": "xcode",
            "include": { "paths": ["a.xcodeproj"] },
            "variables": [
                { "name": "scheme", "label": "S", "required": true, "default": "R" }
            ],
            "build": { "steps": ["x"], "artifactPath": "p", "artifacts": ["q"] },
            "cache": { "paths": ["c"] }
        });
        let mut keys = Vec::new();
        collect_keys(&full, &mut keys);

        for key in keys {
            assert!(
                properties.contains(&key),
                "field `{key}` from the instance is missing from the schema; schema properties = {properties:?}"
            );
        }
    }


    /// Schema constraints must match what `Manifest::validate()` actually rejects — the
    /// "logic" half of §2 "the schema always agrees with the parsing logic".
    #[test]
    fn schema_states_the_constraints_validate_enforces() {
        let schema = json_schema();
        assert_eq!(schema["$schema"], "http://json-schema.org/draft-07/schema#");
        assert_eq!(schema["type"], "object");

        // Missing displayName / build is rejected by validate()
        let required: Vec<String> = schema["required"]
            .as_array()
            .expect("the root schema must have required")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(required.contains(&"displayName".to_string()), "{required:?}");
        assert!(required.contains(&"build".to_string()), "{required:?}");
        // Optional fields must not leak into required, or the schema is stricter than the code
        for optional in ["variables", "cache", "platform", "include"] {
            assert!(!required.contains(&optional.to_string()), "{required:?}");
        }

        // An empty build.steps array is rejected by validate()
        let build = &schema["definitions"]["ManifestBuild"];
        let build_required: Vec<&str> = build["required"]
            .as_array()
            .expect("ManifestBuild must have required")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(build_required, vec!["steps"]);
        assert_eq!(build["properties"]["steps"]["minItems"], 1);
        assert_eq!(build["properties"]["steps"]["type"], "array");

        // An empty or missing variables[].name is rejected by validate()
        let variable = &schema["definitions"]["ManifestVariable"];
        assert_eq!(variable["required"], serde_json::json!(["name"]));
        assert_eq!(variable["properties"]["name"]["minLength"], 1);
        // default is a nullable Option<String>
        assert_eq!(
            variable["properties"]["default"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    /// The shipped templates are the most honest samples of this schema: each must pass its
    /// own constraints.
    #[test]
    fn shipped_templates_satisfy_the_generated_schema() {
        let schema = json_schema();
        for id in &["xcode", "flutter-ios", "gradle-android", "kotlin-multiplatform-ios"] {
            let raw = template(id).unwrap_or_else(|| panic!("missing template {id}"));
            let manifest = parse(raw).unwrap_or_else(|err| panic!("failed to parse {id}: {err}"));

            // Structurally: every schema-required field must be present
            for field in schema["required"].as_array().unwrap() {
                let field = field.as_str().unwrap();
                let serialized = serde_json::to_value(&manifest).unwrap();
                assert!(
                    serialized.get(field).is_some(),
                    "template {id} lacks schema-required field {field}"
                );
                if field == "displayName" {
                    assert!(!manifest.display_name.trim().is_empty(), "{id}");
                }
            }
            assert!(!manifest.build.steps.is_empty(), "template {id} has an empty build.steps");
            assert!(manifest.validate().is_empty(), "template {id} fails its own validation");
        }
    }
}
