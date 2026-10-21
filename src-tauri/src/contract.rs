//! Task input/output contract (design doc §7.6.3 / §7.6.4 / §7.6.7).
//!
//! Three steps, all here:
//!
//! 1. **Resolve inputs** -- `{{inputs.<name>}}` values come from `Task::variables`,
//!    else the declared `default`; `enum` / `bool` / `number` shapes are checked too.
//! 2. **Expand** -- replace `{{inputs.x}}` in the task's **path and name fields**, then
//!    **re-normalize and re-check the relative paths** (§7.6.4 demands it: the values
//!    are user input, so a `..`-free original JSON proves nothing).
//! 3. **Resolve outputs** -- turn declarations into "which side, which path, file or
//!    directory". The actual existence check lives in `server.rs` (the remote side needs SSH).
//!
//! **Only path/name fields are touched, never the shell commands in `build.steps`**:
//! command substitution and escaping belong to `manifest`, and the two syntaxes must not mix.
//!
//! Output contract (§7.6.3): *"Task success cannot rest on the shell exit code alone"* --
//! a declared output must be confirmed present and of the right type after the run.

use std::collections::BTreeMap;

use crate::paths::{self, Location, Side};
use crate::projects::{
    Task, INPUT_NAMESPACE, KIND_COPY, KIND_DELETE, KIND_EXPORT, KIND_MOVE, KIND_PULL,
};

/// One resolved output declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOutput {
    /// Name from the declaration (referenced as `{{steps.<id>.outputs.<name>}}`).
    pub name: String,
    pub side: Side,
    /// Expanded relative path.
    pub path: String,
    /// Directory or file.
    pub directory: bool,
    /// Whether a missing output counts as failure.
    pub required: bool,
}

/// Every task field that **gets expanded**, as (label, value). Read-only, no task clone.
fn templated(task: &Task) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = vec![
        ("artifactPath".to_string(), task.artifact_path.clone()),
        ("from.path".to_string(), task.from.path.clone()),
        ("to.path".to_string(), task.to.path.clone()),
        ("target.path".to_string(), task.target.path.clone()),
        ("outputName".to_string(), task.output_name.clone()),
    ];
    for (index, item) in task.artifacts.iter().enumerate() {
        fields.push((format!("artifacts[{index}]"), item.clone()));
    }
    for (index, item) in task.sources.iter().enumerate() {
        fields.push((format!("sources[{index}].path"), item.path.clone()));
    }
    fields
}

/// Which input names are referenced (deduplicated, first-seen order).
pub fn referenced_inputs(task: &Task) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for (_, value) in templated(task) {
        candidates.extend(placeholders(&value));
    }
    for spec in task.outputs.values() {
        candidates.extend(placeholders(&spec.path));
    }
    let mut names: Vec<String> = Vec::new();
    for name in candidates {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Extract `{{inputs.<name>}}` names from text (other placeholders belong to `manifest`).
pub fn placeholders(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let key = after[..end].trim();
        if let Some(name) = key.strip_prefix(INPUT_NAMESPACE) {
            let name = name.trim();
            if !name.is_empty() && !names.contains(&name.to_string()) {
                names.push(name.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    names
}

/// Single-pass replacement of `{{inputs.x}}`.
///
/// **No repeated expansion**: a `{{...}}` inside a substituted value stays plain text,
/// otherwise one variable's value could inject another placeholder.
pub fn expand(text: &str, inputs: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("占位符没有闭合:{text}"));
        };
        let key = after[..end].trim();
        if let Some(name) = key.strip_prefix(INPUT_NAMESPACE) {
            let name = name.trim();
            match inputs.get(name) {
                Some(value) => out.push_str(value),
                None => return Err(format!("{{{{{key}}}}} 引用了没有声明的输入:{name}")),
            }
        } else {
            // Placeholders outside the inputs namespace stay as-is (manifest variables
            // are handled during the build)
            out.push_str(&rest[start..start + 2 + end + 2]);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolve input values and validate their shape.
///
/// `snapshot` holds the values **used by the previous run of this job** (design doc
/// §7.6.7): a retry prefers them, so editing task variables and retrying does not
/// silently swap in a different set of inputs, keeping logs and results auditable.
pub fn resolve_inputs(
    task: &Task,
    snapshot: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, Vec<String>> {
    let mut errors = Vec::new();
    let mut out = BTreeMap::new();

    for (name, spec) in &task.inputs {
        if name.trim().is_empty() {
            errors.push("输入的名字不能为空".to_string());
            continue;
        }
        let value = snapshot
            .and_then(|map| map.get(name))
            .or_else(|| task.variables.get(name))
            .cloned()
            .or_else(|| spec.default.clone())
            .unwrap_or_default();

        let kind = spec.r#type.trim().to_ascii_lowercase();
        if !spec.values.is_empty() && !value.is_empty() && !spec.values.contains(&value) {
            errors.push(format!(
                "输入 {name} 的取值不合法:{value}(可选:{})",
                spec.values.join(" / ")
            ));
        }
        match kind.as_str() {
            "" | "string" | "enum" | "path" => {}
            "bool" => {
                if !value.is_empty() && !matches!(value.as_str(), "true" | "false" | "1" | "0") {
                    errors.push(format!("输入 {name} 不是布尔值:{value}"));
                }
            }
            "number" => {
                if !value.is_empty() && value.parse::<f64>().is_err() {
                    errors.push(format!("输入 {name} 不是数字:{value}"));
                }
            }
            other => errors.push(format!(
                "输入 {name} 的类型不认识:{other}(可用:string / enum / path / bool / number)"
            )),
        }
        out.insert(name.clone(), value);
    }

    for name in referenced_inputs(task) {
        if !task.inputs.contains_key(&name) {
            errors.push(format!(
                "{{{{inputs.{name}}}}} 引用了没有声明的输入(在任务的 inputs 里声明它)"
            ));
        }
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Expand one controlled location (expand + re-normalize + boundary check).
fn resolve_location(
    label: &str,
    location: &Location,
    inputs: &BTreeMap<String, String>,
    errors: &mut Vec<String>,
) -> Location {
    let expanded = match expand(&location.path, inputs) {
        Ok(value) => value,
        Err(err) => {
            errors.push(format!("{label}:{err}"));
            return location.clone();
        }
    };
    match paths::normalize_relative(&expanded) {
        Ok(value) if !value.is_empty() => {
            let mut fixed = location.clone();
            fixed.path = value;
            fixed
        }
        Ok(_) => {
            errors.push(format!("{label} 展开后成了项目根目录本身"));
            location.clone()
        }
        Err(err) => {
            errors.push(format!("{label} 展开后不合法:{err}"));
            location.clone()
        }
    }
}

/// Expand every path/name field of a task and **re-validate paths after expanding**.
///
/// Returns an executable copy; `task` itself is untouched (the stored one keeps its
/// templates).
pub fn resolved(task: &Task, inputs: &BTreeMap<String, String>) -> Result<Task, Vec<String>> {
    let mut out = task.clone();
    let mut errors: Vec<String> = Vec::new();

    let expand_field = |label: &str, raw: &str, errors: &mut Vec<String>| -> String {
        match expand(raw, inputs) {
            Ok(value) => value,
            Err(err) => {
                errors.push(format!("{label}:{err}"));
                raw.to_string()
            }
        }
    };

    out.artifact_path = expand_field("artifactPath", &task.artifact_path, &mut errors);
    out.artifacts = task
        .artifacts
        .iter()
        .map(|item| expand_field("artifacts", item, &mut errors))
        .collect();
    out.output_name = expand_field("outputName", &task.output_name, &mut errors);

    // Controlled locations must be **re**-normalized and re-checked after expanding (§7.6.4).
    //
    // **Only fields this kind actually uses are touched**: an export task has no
    // `to`/`target` and build / push have none at all, so normalizing empty strings would
    // only produce a pile of false errors.
    out.sources = match out.kind() {
        KIND_PULL | KIND_EXPORT => task
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                resolve_location(&format!("sources[{index}]"), source, inputs, &mut errors)
            })
            .collect(),
        _ => task.sources.clone(),
    };
    match out.kind() {
        KIND_COPY | KIND_MOVE => {
            out.from = resolve_location("from", &task.from, inputs, &mut errors);
            out.to = resolve_location("to", &task.to, inputs, &mut errors);
        }
        KIND_PULL => {
            out.to = resolve_location("to", &task.to, inputs, &mut errors);
        }
        KIND_DELETE => {
            out.target = resolve_location("target", &task.target, inputs, &mut errors);
        }
        _ => {}
    }

    // Artifact paths may be absolute or `~` (the build uses them as-is), but never contain `..`
    if out.artifact_path.contains("..") {
        errors.push(format!(
            "artifactPath 展开后不允许出现 ..:{}",
            out.artifact_path
        ));
    }
    for item in &out.artifacts {
        if item.contains("..") {
            errors.push(format!("artifacts 展开后不允许出现 ..:{item}"));
        }
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Expand declared outputs into "which side to check, at which path".
pub fn resolved_outputs(
    task: &Task,
    inputs: &BTreeMap<String, String>,
) -> Result<Vec<DeclaredOutput>, Vec<String>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (name, spec) in &task.outputs {
        let expanded = match expand(&spec.path, inputs) {
            Ok(value) => value,
            Err(err) => {
                errors.push(format!("输出 {name}:{err}"));
                continue;
            }
        };
        match paths::normalize_relative(&expanded) {
            Ok(value) if !value.is_empty() => out.push(DeclaredOutput {
                name: name.clone(),
                side: spec.side,
                path: value,
                directory: spec.wants_directory(),
                required: spec.is_required(),
            }),
            Ok(_) => errors.push(format!("输出 {name} 不能是项目根目录本身")),
            Err(err) => errors.push(format!("输出 {name} 的路径不合法:{err}")),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Namespace of `{{steps.<step id>.outputs.<name>}}` (design doc §7.6.5).
pub const STEPS_NAMESPACE: &str = "steps.";

/// Extract the step outputs a text references, as `"<step id>.outputs.<name>"`.
pub fn step_references(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let key = after[..end].trim();
        if let Some(rest_key) = key.strip_prefix(STEPS_NAMESPACE) {
            let rest_key = rest_key.trim();
            if !rest_key.is_empty() && !names.contains(&rest_key.to_string()) {
                names.push(rest_key.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    names
}

/// Expand `{{steps.<step id>.outputs.<name>}}` (design doc §7.6.5).
///
/// Keys of `produced` are `"<step id>.outputs.<name>"` and values are the earlier step's
/// **verified** real path (with no `host:` / `remote:` prefix, so it works directly as a
/// path value). An unresolvable reference is an error, never a guess: a guessed path
/// hides the failure far away from its cause.
pub fn expand_steps(text: &str, produced: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("占位符没有闭合:{text}"));
        };
        let key = after[..end].trim();
        if let Some(name) = key.strip_prefix(STEPS_NAMESPACE) {
            let name = name.trim();
            match produced.get(name) {
                Some(value) => out.push_str(value),
                None => {
                    return Err(format!(
                        "{{{{{key}}}}} 引用不到:要么这一步在它前面没有跑成功,要么名字写错了"
                    ))
                }
            }
        } else {
            out.push_str(&rest[start..start + 2 + end + 2]);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Snapshot key: `<taskId>.<input name>` (design doc §7.6.7).
///
/// One job may run several tasks (one per batch step), so the snapshot is keyed per task
/// and each task reads back only its own slice.
pub fn snapshot_key(task_id: &str, name: &str) -> String {
    format!("{task_id}.{name}")
}

/// Merge one task's resolved inputs into the whole-job snapshot.
pub fn merge_snapshot(
    all: &mut BTreeMap<String, String>,
    task_id: &str,
    values: &BTreeMap<String, String>,
) {
    for (name, value) in values {
        all.insert(snapshot_key(task_id, name), value.clone());
    }
}

/// Take one task's slice out of the job snapshot (used to restore inputs on retry).
pub fn snapshot_for(task_id: &str, all: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let prefix = format!("{task_id}.");
    all.iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(&prefix)
                .map(|name| (name.to_string(), value.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{InputSpec, OutputSpec, Task};

    fn task_with(inputs: &[(&str, InputSpec)], outputs: &[(&str, OutputSpec)]) -> Task {
        let mut task = Task {
            id: "t-1".to_string(),
            kind: "export".to_string(),
            ..Task::default()
        };
        for (name, spec) in inputs {
            task.inputs.insert(name.to_string(), spec.clone());
        }
        for (name, spec) in outputs {
            task.outputs.insert(name.to_string(), spec.clone());
        }
        task
    }

    fn string_input() -> InputSpec {
        InputSpec {
            r#type: "string".to_string(),
            ..InputSpec::default()
        }
    }

    #[test]
    fn values_come_from_variables_then_default_then_snapshot() {
        let mut task = task_with(&[("dir", string_input())], &[]);
        task.inputs.get_mut("dir").unwrap().default = Some("fallback".to_string());
        // No variable given -> use the default
        let values = resolve_inputs(&task, None).unwrap();
        assert_eq!(values.get("dir").unwrap(), "fallback");

        // A variable wins over the default
        task.variables
            .insert("dir".to_string(), "artifacts".to_string());
        let values = resolve_inputs(&task, None).unwrap();
        assert_eq!(values.get("dir").unwrap(), "artifacts");

        // The snapshot wins over variables (§7.6.7: a retry reruns that version, not this one)
        let mut snapshot = BTreeMap::new();
        snapshot.insert("dir".to_string(), "original".to_string());
        let values = resolve_inputs(&task, Some(&snapshot)).unwrap();
        assert_eq!(values.get("dir").unwrap(), "original");
    }

    #[test]
    fn undeclared_reference_and_bad_enum_are_rejected() {
        // Reference to an undeclared input
        let mut task = task_with(&[], &[]);
        task.output_name = "{{inputs.nope}}.zip".to_string();
        let errors = resolve_inputs(&task, None).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("没有声明的输入")),
            "{errors:?}"
        );

        // enum value outside the allowed set
        let mut spec = string_input();
        spec.r#type = "enum".to_string();
        spec.values = vec!["Release".to_string(), "Debug".to_string()];
        let mut task = task_with(&[("configuration", spec)], &[]);
        task.variables
            .insert("configuration".to_string(), "Bogus".to_string());
        let errors = resolve_inputs(&task, None).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("取值不合法")), "{errors:?}");
    }

    #[test]
    fn expansion_is_single_pass_and_rejects_unknown() {
        let mut inputs = BTreeMap::new();
        inputs.insert("a".to_string(), "{{inputs.b}}".to_string());
        inputs.insert("b".to_string(), "boom".to_string());
        // A placeholder inside a value is not expanded again
        assert_eq!(expand("x-{{inputs.a}}", &inputs).unwrap(), "x-{{inputs.b}}");

        assert!(expand("{{inputs.missing}}", &inputs).is_err());
        assert!(expand("{{inputs.a", &inputs).is_err());
        // Manifest placeholders stay as-is
        assert_eq!(expand("{{scheme}}", &inputs).unwrap(), "{{scheme}}");
    }

    #[test]
    fn expanded_paths_are_revalidated() {
        let mut spec = string_input();
        spec.r#type = "path".to_string();
        spec.side = Some(Side::Host);
        let mut task = task_with(&[("dir", spec)], &[]);
        // Use copy: its from gets expanded and checked (export never looks at from)
        task.kind = KIND_COPY.to_string();
        task.from = Location {
            side: Side::Host,
            path: "{{inputs.dir}}".to_string(),
        };
        task.to = Location {
            side: Side::Host,
            path: "dst".to_string(),
        };

        // A value containing `..` must be rejected after expanding (§7.6.4)
        task.variables
            .insert("dir".to_string(), "../escape".to_string());
        let values = resolve_inputs(&task, None).unwrap();
        let errors = resolved(&task, &values).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("..")), "{errors:?}");

        // A normal value -> expanded and normalized
        task.variables
            .insert("dir".to_string(), "./src/".to_string());
        let values = resolve_inputs(&task, None).unwrap();
        let resolved_task = resolved(&task, &values).unwrap();
        assert_eq!(resolved_task.from.path, "src");
    }

    #[test]
    fn step_output_references_expand_from_earlier_steps() {
        let mut produced = BTreeMap::new();
        produced.insert(
            "package.outputs.ipa".to_string(),
            ".cgw/ipa/App.ipa".to_string(),
        );

        assert_eq!(step_references("{{steps.package.outputs.ipa}}"), vec![
            "package.outputs.ipa".to_string()
        ]);
        assert_eq!(
            expand_steps("dir={{steps.package.outputs.ipa}}", &produced).unwrap(),
            "dir=.cgw/ipa/App.ipa"
        );
        // An unresolvable reference errors instead of guessing
        let err = expand_steps("{{steps.nope.outputs.x}}", &produced).unwrap_err();
        assert!(err.contains("引用不到"), "{err}");
        // `{{inputs.x}}` is not handled here and stays as-is
        assert_eq!(
            expand_steps("{{inputs.x}}", &produced).unwrap(),
            "{{inputs.x}}"
        );
    }

    #[test]
    fn snapshots_are_keyed_per_task() {
        // Two tasks in one batch have their own input values and must not leak into each other
        let mut all = BTreeMap::new();
        let mut first = BTreeMap::new();
        first.insert("scheme".to_string(), "iosApp".to_string());
        let mut second = BTreeMap::new();
        second.insert("scheme".to_string(), "otherApp".to_string());

        merge_snapshot(&mut all, "t-1", &first);
        merge_snapshot(&mut all, "t-2", &second);
        assert_eq!(all.get("t-1.scheme").unwrap(), "iosApp");
        assert_eq!(all.get("t-2.scheme").unwrap(), "otherApp");

        let back = snapshot_for("t-1", &all);
        assert_eq!(back.get("scheme").unwrap(), "iosApp");
        assert_eq!(back.len(), 1);
        assert!(snapshot_for("t-3", &all).is_empty());
    }

    #[test]
    fn declared_outputs_are_expanded_and_typed() {
        let mut spec = string_input();
        spec.r#type = "path".to_string();
        let mut task = task_with(&[("dir", spec)], &[]);
        task.variables
            .insert("dir".to_string(), "artifacts".to_string());
        task.outputs.insert(
            "ipa".to_string(),
            OutputSpec {
                r#type: "file".to_string(),
                side: Side::Host,
                path: "{{inputs.dir}}/app.ipa".to_string(),
                required: None,
            },
        );
        task.outputs.insert(
            "logs".to_string(),
            OutputSpec {
                r#type: "directory".to_string(),
                side: Side::Remote,
                path: "build/logs".to_string(),
                required: Some(false),
            },
        );

        let values = resolve_inputs(&task, None).unwrap();
        let mut declared = resolved_outputs(&task, &values).unwrap();
        declared.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].name, "ipa");
        assert_eq!(declared[0].path, "artifacts/app.ipa");
        assert!(!declared[0].directory);
        assert!(declared[0].required, "a missing `required` means required");
        assert_eq!(declared[1].name, "logs");
        assert!(declared[1].directory);
        assert!(!declared[1].required);
    }
}
