//! Cross-project workflow presets (design doc §14.3 / acceptance notes §6.3).
//!
//! **A preset is a template, not runtime state**: tasks and batches in the project JSON
//! stay authoritative. `apply` instantiates a preset into a project, after which it is
//! exactly like hand-built tasks and batches and runs from the CLI / WebUI without the AI.
//!
//! Stored symmetrically with `manifest/`: `<config.json dir>/workflows/<id>.json`. The
//! first run seeds the shipped templates and then **never touches the directory again**.
//!
//! The four questions of design doc §14.3 are settled here:
//!
//! 1. **Parameters** come from `variables` (name / default / required) and apply may
//!    override them; only **declared** `{{name}}` placeholders are substituted, the rest
//!    fall through to the manifest variables / task `inputs` layer, so template parameters
//!    and build variables never swallow each other.
//! 2. **Id collisions are real**: instantiation keeps the preset's task ids, so a
//!    duplicate is refused with the conflict list (preview first); `overwrite` replaces.
//! 3. **The project JSON is authoritative**: apply is one-way (preset -> project), with no
//!    reverse sync, so editing a preset never affects projects already applied.
//! 4. **Writers**: the directory is a user file like `manifest/`, guarded by the daemon's
//!    usual `authorized()` check, and can also be edited on disk directly.
//!
//! No `reload`/`refresh`: `manifest/` needs one because it fingerprints files to report
//! differences, while nothing is cached here -- the directory is read on every call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::projects::{Batch, BatchStep, Project, Task};
use crate::server::TaskInput;

pub const DIR_NAME: &str = "workflows";

/// One parameter a preset declares.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename = "WorkflowVariable", extend("required" = ["name"]))]
pub struct WorkflowVariable {
    /// Placeholder name, written `{{name}}` in the preset. Unique within one preset.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Label shown in the UI; empty falls back to `name`.
    pub label: String,
    /// Required: apply is refused when no value (and no default) is given.
    pub required: bool,
    pub default: Option<String>,
}

/// One cross-project workflow preset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(extend("required" = ["name", "tasks", "steps"]))]
pub struct WorkflowPreset {
    /// Display name.
    #[schemars(length(min = 1))]
    pub name: String,
    pub description: String,
    /// Parameters (name / default / required).
    pub variables: Vec<WorkflowVariable>,
    /// Inline task templates. **The author's ids are kept**, so id collisions surface.
    ///
    /// Uses `TaskInput` rather than `projects::Task`: **it is the one source of the task
    /// schema** (`cgw://schema/task` is generated from it too), so "a task in a preset" and
    /// "a task the API accepts" cannot drift into two different shapes.
    pub tasks: Vec<TaskInput>,
    /// Ordered steps referencing an id above; `id` is optional, and when set later steps
    /// can reference this step's outputs.
    pub steps: Vec<BatchStep>,
}

impl WorkflowPreset {
    /// Static validation; the expanded result is checked again in [`plan`].
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.name.trim().is_empty() {
            errors.push("name 不能为空".to_string());
        }

        let mut names: Vec<&str> = Vec::new();
        for variable in &self.variables {
            let name = variable.name.trim();
            if name.is_empty() {
                errors.push("变量名不能为空".to_string());
            } else if names.contains(&name) {
                errors.push(format!("变量名重复:{name}"));
            } else {
                names.push(name);
            }
        }

        if self.tasks.is_empty() {
            errors.push("至少要有一个任务模板".to_string());
        }
        let tasks = tasks_of(self);
        let mut task_ids: Vec<&str> = Vec::new();
        for task in &tasks {
            let id = task.id.as_str();
            if id.is_empty() {
                errors.push("任务模板缺少 id(实例化时要靠它对上步骤)".to_string());
                continue;
            }
            if task_ids.contains(&id) {
                errors.push(format!("任务 id 重复:{id}"));
                continue;
            }
            task_ids.push(id);
            for problem in task.validate() {
                errors.push(format!("任务 {id}:{problem}"));
            }
        }

        // Step rules are **identical** to a project batch (both call workflow::validate_steps):
        // non-empty, references exist, step ids unique, output references only to outputs
        // declared by an earlier step.
        if let Err((_, message)) = validate_steps(&tasks, &self.steps) {
            errors.push(message);
        }

        errors
    }

    fn variable(&self, name: &str) -> Option<&WorkflowVariable> {
        self.variables.iter().find(|item| item.name == name)
    }
}

/// Preset tasks -> domain model.
///
/// `TaskInput` is the API/schema shape and `Task` is the shape that actually runs; their
/// serde field names match and the conversion goes through [`TaskInput::into_task`] -- the
/// very same path HTTP task saving takes.
fn tasks_of(preset: &WorkflowPreset) -> Vec<Task> {
    preset
        .tasks
        .iter()
        .cloned()
        .map(|input| {
            let id = input.id.trim().to_string();
            input.into_task(id)
        })
        .collect()
}

/// Validation rules for batch steps (design doc §7.3 / §7.6.5).
///
/// **Project batches and workflow presets share this one copy**: two separate
/// implementations would sooner or later contradict each other ("saves into the project,
/// refuses to apply"). Returns `(error code, message)`.
pub fn validate_steps(tasks: &[Task], steps: &[BatchStep]) -> Result<(), (&'static str, String)> {
    if steps.is_empty() {
        return Err(("batchInvalid", "batch 至少要有一个步骤".to_string()));
    }
    for step in steps {
        if step.task_id.trim().is_empty() {
            return Err((
                "batchInvalid",
                "步骤里的 taskId 不能为空".to_string(),
            ));
        }
        if !tasks.iter().any(|task| task.id == step.task_id) {
            return Err((
                "taskNotFound",
                format!("步骤引用的任务不存在:{}", step.task_id),
            ));
        }
    }

    let mut seen: Vec<&str> = Vec::new();
    for step in steps {
        let id = step.id.trim();
        if id.is_empty() {
            continue;
        }
        if seen.contains(&id) {
            return Err(("batchInvalid", format!("步骤 id 重复:{id}")));
        }
        seen.push(id);
    }

    for (index, step) in steps.iter().enumerate() {
        for (key, value) in &step.variables {
            for reference in crate::contract::step_references(value) {
                let Some((source_id, output_name)) = reference.split_once(".outputs.") else {
                    return Err((
                        "batchInvalid",
                        format!(
                            "步骤输出引用的写法不对:{reference}\
                             (要写 {{{{steps.<步骤 id>.outputs.<输出名>}}}})"
                        ),
                    ));
                };
                let Some(source_index) = steps
                    .iter()
                    .position(|other| other.id.trim() == source_id)
                else {
                    return Err((
                        "batchInvalid",
                        format!("步骤 {} 引用了不存在的步骤 id:{source_id}", index + 1),
                    ));
                };
                if source_index >= index {
                    return Err((
                        "batchInvalid",
                        format!(
                            "步骤 {} 引用了它自己或者后面的步骤({source_id});\
                             只能引用前面已经跑过的步骤",
                            index + 1
                        ),
                    ));
                }
                let source_task = tasks
                    .iter()
                    .find(|task| task.id == steps[source_index].task_id)
                    .ok_or_else(|| {
                        (
                            "taskNotFound",
                            format!("步骤引用的任务不存在:{}", steps[source_index].task_id),
                        )
                    })?;
                let Some(output) = source_task.outputs.get(output_name) else {
                    return Err((
                        "batchInvalid",
                        format!(
                            "步骤 {} 引用了 {source_id} 没有声明的输出:{output_name}",
                            index + 1
                        ),
                    ));
                };
                // Side check: a consuming input that declares a side must match the producer
                if let Some(spec) = tasks
                    .iter()
                    .find(|task| task.id == step.task_id)
                    .and_then(|task| task.inputs.get(key))
                {
                    if let Some(side) = spec.side {
                        if side != output.side {
                            return Err((
                                "batchInvalid",
                                format!(
                                    "步骤 {} 的输入 {key} 声明在 {} 侧,\
                                     但 {source_id}.outputs.{output_name} 在 {} 侧",
                                    index + 1,
                                    side.code(),
                                    output.side.code()
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolve parameter values: override first, then default; a missing required one errors.
pub fn resolve_variables(
    preset: &WorkflowPreset,
    overrides: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, Vec<String>> {
    let mut errors = Vec::new();
    let mut values = BTreeMap::new();
    for variable in &preset.variables {
        let name = variable.name.trim();
        let value = overrides
            .get(name)
            .cloned()
            .or_else(|| variable.default.clone())
            .unwrap_or_default();
        if variable.required && value.is_empty() {
            errors.push(format!("参数 {name} 是必填的,apply 时要给值"));
        }
        values.insert(name.to_string(), value);
    }
    // An override for an undeclared parameter errors, so a no-op edit is not mistaken for a change
    for name in overrides.keys() {
        if preset.variable(name).is_none() {
            errors.push(format!("模板里没有声明参数 {name}"));
        }
    }
    if errors.is_empty() {
        Ok(values)
    } else {
        Err(errors)
    }
}

/// Single-pass replacement of **declared** `{{name}}`; other placeholders stay as-is (they
/// belong to manifest variables or task `inputs`, resolved by the next layer). No repeated
/// expansion.
fn expand_declared(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // Unclosed: leave as-is and let the next layer report it
            out.push_str(rest);
            return out;
        };
        let key = after[..end].trim();
        match vars.get(key) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Recursively substitute `{{declared names}}` in every JSON string.
///
/// Working over JSON rather than field by field, because task fields are numerous and
/// nested (`from.path` and friends): a missed field shows up only as "the parameter did
/// nothing here", which is hard to trace.
fn substitute(value: &mut Value, vars: &BTreeMap<String, String>) {
    match value {
        Value::String(text) => *text = expand_declared(text, vars),
        Value::Array(items) => {
            for item in items {
                substitute(item, vars);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                substitute(item, vars);
            }
        }
        _ => {}
    }
}

/// Result of apply: the tasks and batch to write into the project, plus id conflicts found.
pub struct ApplyPlan {
    pub tasks: Vec<Task>,
    pub batch: Batch,
    pub conflicts: Vec<String>,
}

/// Instantiate: substitute parameters -> build tasks and batch -> find id conflicts.
///
/// Plan only, nothing is written: the caller can show the conflict preview first
/// (design doc §14.3 item 2) or execute it directly.
pub fn plan(
    preset_id: &str,
    preset: &WorkflowPreset,
    project: &Project,
    overrides: &BTreeMap<String, String>,
    batch_name: Option<String>,
) -> Result<ApplyPlan, Vec<String>> {
    let values = resolve_variables(preset, overrides)?;

    // Substitution runs over JSON, so convert to the domain model first: substitution and
    // execution then see exactly the same shape
    let mut tasks_value = serde_json::to_value(tasks_of(preset))
        .map_err(|err| vec![format!("任务模板无法序列化:{err}")])?;
    substitute(&mut tasks_value, &values);
    let tasks: Vec<Task> = serde_json::from_value(tasks_value)
        .map_err(|err| vec![format!("替换后任务模板不合法:{err}")])?;

    let mut steps_value = serde_json::to_value(&preset.steps)
        .map_err(|err| vec![format!("步骤无法序列化:{err}")])?;
    substitute(&mut steps_value, &values);
    let steps: Vec<BatchStep> = serde_json::from_value(steps_value)
        .map_err(|err| vec![format!("替换后步骤不合法:{err}")])?;

    let mut errors: Vec<String> = Vec::new();
    for task in &tasks {
        for problem in task.validate() {
            errors.push(format!("任务 {}:{problem}", task.id));
        }
    }
    if let Err((_, message)) = validate_steps(&tasks, &steps) {
        errors.push(message);
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // Id conflicts: the task id or batch id already exists in this project
    let mut conflicts: Vec<String> = Vec::new();
    for task in &tasks {
        if project.task(&task.id).is_some() {
            conflicts.push(format!("任务 {}", task.id));
        }
    }
    let batch_id = batch_id_for(preset_id);
    if project.batch(&batch_id).is_some() {
        conflicts.push(format!("批量工作流 {batch_id}"));
    }

    Ok(ApplyPlan {
        tasks,
        batch: Batch {
            id: batch_id,
            name: batch_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| preset.name.trim().to_string()),
            steps,
        },
        conflicts,
    })
}

/// Applying the same preset again must land on the **same** batch id, so conflicts stay
/// visible and overwriting stays idempotent (otherwise every apply adds a batch).
pub fn batch_id_for(preset_id: &str) -> String {
    format!("wf-{preset_id}")
}

// ------------------------------------------------------------------ disk

pub fn dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DIR_NAME)
}

pub fn path_of(config_path: &Path, id: &str) -> PathBuf {
    dir(config_path).join(format!("{id}.json"))
}

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

pub fn load(config_path: &Path, id: &str) -> Result<WorkflowPreset, String> {
    let path = path_of(config_path, id);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&raw)
}

pub fn raw(config_path: &Path, id: &str) -> Result<String, String> {
    let path = path_of(config_path, id);
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(raw: &str) -> Result<WorkflowPreset, String> {
    let preset: WorkflowPreset =
        serde_json::from_str(raw).map_err(|e| format!("不是合法 JSON:{e}"))?;
    let errors = preset.validate();
    if errors.is_empty() {
        Ok(preset)
    } else {
        Err(errors.join("; "))
    }
}

pub fn parse_json(raw: &str) -> (Option<WorkflowPreset>, Vec<String>) {
    match serde_json::from_str::<WorkflowPreset>(raw) {
        Ok(preset) => {
            let errors = preset.validate();
            (Some(preset), errors)
        }
        Err(err) => (None, vec![format!("不是合法 JSON:{err}")]),
    }
}

pub fn write_raw(config_path: &Path, id: &str, raw: &str) -> Result<(), String> {
    let dir = dir(config_path);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = if raw.trim().is_empty() {
        "{}".to_string()
    } else {
        match serde_json::from_str::<Value>(raw) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
            Err(_) => raw.to_string(),
        }
    };
    std::fs::write(path_of(config_path, id), text + "\n").map_err(|e| format!("写入失败:{e}"))
}

pub fn remove(config_path: &Path, id: &str) -> Result<(), String> {
    let path = path_of(config_path, id);
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Shipped templates as `(id, body)`. Used only for first seeding and filling gaps.
pub const TEMPLATES: [(&str, &str); 4] = [
    (
        "push-build-pull",
        include_str!("../templates/workflows/push-build-pull.json"),
    ),
    (
        "push-build-export",
        include_str!("../templates/workflows/push-build-export.json"),
    ),
    (
        "build-export-zip",
        include_str!("../templates/workflows/build-export-zip.json"),
    ),
    (
        "clean-push-build-pull",
        include_str!("../templates/workflows/clean-push-build-pull.json"),
    ),
];

pub fn is_template(id: &str) -> bool {
    TEMPLATES.iter().any(|(name, _)| *name == id)
}

pub fn template(id: &str) -> Option<&'static str> {
    TEMPLATES
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, body)| *body)
}

/// First run: seed only when the directory does not exist; when it does, touch nothing
/// (the same rule as `manifest/`).
pub fn seed_templates(config_path: &Path) -> Prepared {
    let dir = dir(config_path);
    if dir.exists() {
        return Prepared {
            dir,
            created: false,
            seeded: Vec::new(),
        };
    }
    let mut seeded = Vec::new();
    if std::fs::create_dir_all(&dir).is_ok() {
        for (id, body) in TEMPLATES {
            if std::fs::write(path_of(config_path, id), body).is_ok() {
                seeded.push(id);
            }
        }
    }
    Prepared {
        dir,
        created: true,
        seeded,
    }
}

/// Which templates are still missing (used by "fill in the missing templates").
pub fn missing_templates(config_path: &Path) -> Vec<(&'static str, String)> {
    TEMPLATES
        .iter()
        .filter(|(id, _)| !path_of(config_path, id).exists())
        .map(|(id, body)| {
            let name = serde_json::from_str::<WorkflowPreset>(body)
                .map(|preset| preset.name)
                .unwrap_or_else(|_| (*id).to_string());
            (*id, name)
        })
        .collect()
}

pub struct Prepared {
    pub dir: PathBuf,
    pub created: bool,
    pub seeded: Vec<&'static str>,
}

/// Write back the **missing** templates for the given ids (existing files are never
/// touched); returns which ones were actually written.
pub fn restore_templates(config_path: &Path, ids: &[String]) -> Vec<String> {
    let mut restored = Vec::new();
    for id in ids {
        let Some(body) = template(id) else { continue };
        if path_of(config_path, id).exists() {
            continue;
        }
        if std::fs::create_dir_all(dir(config_path)).is_err() {
            continue;
        }
        if std::fs::write(path_of(config_path, id), body).is_ok() {
            restored.push(id.clone());
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{Location, Side};
    use crate::projects::{InputSpec, OutputSpec};

    fn preset_with_pipeline() -> WorkflowPreset {
        // A minimal but complete "push -> build -> pull" preset: build declares an output
        // and pull references it through a step reference
        let mut build = TaskInput {
            id: "t-build".to_string(),
            kind: "build".to_string(),
            name: "构建".to_string(),
            manifest: "{{manifest}}".to_string(),
            artifact_path: "build/{{configuration}}/{{scheme}}.app".to_string(),
            ..TaskInput::default()
        };
        build.outputs.insert(
            "app".to_string(),
            OutputSpec {
                r#type: "directory".to_string(),
                side: Side::Remote,
                path: "build/{{configuration}}/{{scheme}}.app".to_string(),
                required: None,
            },
        );

        let mut pull = TaskInput {
            id: "t-pull".to_string(),
            kind: "pull".to_string(),
            name: "回传".to_string(),
            sources: vec![Location {
                side: Side::Remote,
                path: "{{inputs.source}}".to_string(),
            }],
            to: Location {
                side: Side::Host,
                path: "artifacts".to_string(),
            },
            ..TaskInput::default()
        };
        pull.inputs.insert(
            "source".to_string(),
            InputSpec {
                r#type: "path".to_string(),
                side: Some(Side::Remote),
                ..InputSpec::default()
            },
        );

        WorkflowPreset {
            name: "首次构建".to_string(),
            description: String::new(),
            variables: vec![
                WorkflowVariable {
                    name: "manifest".to_string(),
                    label: "构建类型".to_string(),
                    required: true,
                    default: None,
                },
                // Optional parameter: `configuration` is **deliberately not declared** --
                // it is left to the manifest variable layer
                WorkflowVariable {
                    name: "scheme".to_string(),
                    label: "Scheme".to_string(),
                    required: false,
                    default: Some("App".to_string()),
                },
            ],
            tasks: vec![
                TaskInput {
                    id: "t-push".to_string(),
                    kind: "push".to_string(),
                    name: "推送".to_string(),
                    ..TaskInput::default()
                },
                build,
                pull,
            ],
            steps: vec![
                BatchStep {
                    task_id: "t-push".to_string(),
                    id: "push".to_string(),
                    variables: BTreeMap::new(),
                },
                BatchStep {
                    task_id: "t-build".to_string(),
                    id: "build".to_string(),
                    variables: BTreeMap::new(),
                },
                BatchStep {
                    task_id: "t-pull".to_string(),
                    id: "pull".to_string(),
                    variables: BTreeMap::new(),
                },
            ],
        }
    }

    #[test]
    fn templates_ship_valid() {
        for (id, body) in TEMPLATES {
            let preset = parse(body).unwrap_or_else(|err| panic!("template {id} is invalid: {err}"));
            assert!(!preset.steps.is_empty(), "template {id} has no steps");
            assert!(preset.tasks.len() >= 2, "template {id} is too thin");
        }
    }

    #[test]
    fn required_variables_and_unknown_overrides_are_reported() {
        let preset = preset_with_pipeline();
        let errors = resolve_variables(&preset, &BTreeMap::new()).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("必填")), "{errors:?}");

        let mut overrides = BTreeMap::new();
        overrides.insert("manifest".to_string(), "xcode".to_string());
        assert!(resolve_variables(&preset, &overrides).is_ok());

        overrides.insert("nope".to_string(), "x".to_string());
        let errors = resolve_variables(&preset, &overrides).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("没有声明")), "{errors:?}");
    }

    #[test]
    fn declared_variables_are_substituted_and_others_left_alone() {
        let mut values = BTreeMap::new();
        values.insert("scheme".to_string(), "Runner".to_string());
        assert_eq!(expand_declared("{{scheme}}.app", &values), "Runner.app");
        // Undeclared placeholders stay as-is: they belong to the manifest variable layer
        assert_eq!(expand_declared("{{configuration}}", &values), "{{configuration}}");
        // A placeholder inside a value is not expanded again
        let mut nested = BTreeMap::new();
        nested.insert("a".to_string(), "{{scheme}}".to_string());
        assert_eq!(expand_declared("{{a}}", &nested), "{{scheme}}");
    }

    #[test]
    fn plan_instantiates_and_finds_conflicts() {
        let preset = preset_with_pipeline();
        let mut overrides = BTreeMap::new();
        overrides.insert("manifest".to_string(), "xcode".to_string());
        overrides.insert("scheme".to_string(), "Runner".to_string());

        let empty = Project::default();
        let first = plan("first-build", &preset, &empty, &overrides, None).unwrap();
        assert!(first.conflicts.is_empty());
        assert_eq!(first.tasks.len(), 3);
        // `{{manifest}}` was substituted into the task fields
        let build = first.tasks.iter().find(|t| t.id == "t-build").unwrap();
        assert_eq!(build.manifest, "xcode");
        // Declared variables also apply in output paths; undeclared ones stay for the manifest layer
        assert_eq!(
            build.outputs.get("app").unwrap().path,
            "build/{{configuration}}/Runner.app"
        );
        // The batch id is **deterministic**: repeated applies land on the same one
        assert_eq!(first.batch.id, batch_id_for("first-build"));
        assert_eq!(first.batch.steps.len(), 3);
        assert_eq!(first.batch.name, "首次构建");

        // A task with the same id already exists -> conflict (no auto-rename; the user decides)
        let mut project = Project::default();
        project.tasks.push(first.tasks[0].clone());
        let again = plan("first-build", &preset, &project, &overrides, None).unwrap();
        assert_eq!(again.conflicts.len(), 1);
        assert!(again.conflicts[0].contains("t-push"), "{:?}", again.conflicts);
    }

    #[test]
    fn steps_rules_are_shared_with_project_batches() {
        let preset = preset_with_pipeline();
        assert!(preset.validate().is_empty(), "{:?}", preset.validate());

        // Duplicate step id
        let mut broken = preset.clone();
        broken.steps[1].id = "push".to_string();
        assert!(broken.validate().iter().any(|e| e.contains("步骤 id 重复")));

        // Reference to a later step
        let mut broken = preset.clone();
        broken.steps[2].variables.insert(
            "source".to_string(),
            "{{steps.pull.outputs.app}}".to_string(),
        );
        assert!(broken
            .validate()
            .iter()
            .any(|e| e.contains("后面的步骤")));

        // Reference to an output nobody declared
        let mut broken = preset.clone();
        broken.steps[2].variables.insert(
            "source".to_string(),
            "{{steps.build.outputs.nope}}".to_string(),
        );
        assert!(broken
            .validate()
            .iter()
            .any(|e| e.contains("没有声明的输出")));

        // Referenced step is gone
        let mut broken = preset.clone();
        broken.steps.pop();
        broken.steps[0].task_id = "t-nope".to_string();
        assert!(broken
            .validate()
            .iter()
            .any(|e| e.contains("引用的任务不存在")));
    }

    #[test]
    fn seeding_is_one_shot_and_reset_only_fills_gaps() {
        let dir = std::env::temp_dir().join("cgw-workflow-seed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");

        let first = seed_templates(&config);
        assert!(first.created);
        assert_eq!(first.seeded.len(), TEMPLATES.len());
        assert_eq!(list_ids(&config).len(), TEMPLATES.len());
        assert!(missing_templates(&config).is_empty());

        // The user edited or deleted one; the second start must not touch it
        std::fs::write(path_of(&config, "push-build-pull"), "{\"name\":\"我改的\"}").unwrap();
        remove(&config, "build-export-zip").unwrap();
        let second = seed_templates(&config);
        assert!(!second.created);
        assert_eq!(
            std::fs::read_to_string(path_of(&config, "push-build-pull")).unwrap(),
            "{\"name\":\"我改的\"}"
        );

        // Filling gaps writes only the missing file and never touches edited ones
        let missing = missing_templates(&config);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "build-export-zip");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
