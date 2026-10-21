//! Shared helpers for generating JSON Schema from the Rust structs (design doc §2).
//!
//! All three schemas come from here:
//!
//! | name | struct | users |
//! |---|---|---|
//! | `manifest` | [`crate::manifest::Manifest`] | `cgw manifests schema` / MCP resource |
//! | `task` | `server::TaskInput` | `cgw schema task` / MCP resource |
//! | `batch` | `server::BatchInput` | `cgw schema batch` / MCP resource |
//!
//! Generating from the structs instead of hand-writing JSON, because a hand-written copy
//! drifts from the parser sooner or later, and an agent building against a stale schema
//! produces errors that are hard to trace. Field names and optionality are derived from
//! the serde attributes here, so they **cannot** drift.
//!
//! The result also passes through [`tidy`]; see its comment for why.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Value};

use crate::projects;

/// Generate a draft-07 schema and attach `title` / `description`.
///
/// draft-07 rather than schemars' default 2020-12: draft-07 is the common denominator
/// that ajv 7/8, `python-jsonschema`, VS Code and IntelliJ accept out of the box, and
/// these schemas are nowhere near needing 2020-12 features (`mcp/test/schema.mjs`
/// validates with ajv).
pub fn draft07<T: JsonSchema + ?Sized>(title: &str, description: &str) -> Value {
    let mut value = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    if let Some(object) = value.as_object_mut() {
        object.insert("title".to_string(), json!(title));
        object.insert("description".to_string(), json!(description));
    }
    tidy(&mut value);
    value
}

/// Available names in fixed order (the CLI and the API list them this way).
pub const NAMES: [&str; 6] = [
    "manifest",
    "task",
    "batch",
    "client",
    "project",
    "workflow",
];

/// Name -> schema; an unknown name gives `None`.
pub fn named(name: &str) -> Option<Value> {
    match name {
        "manifest" => Some(crate::manifest::json_schema()),
        "task" => Some(task()),
        "batch" => Some(batch()),
        "client" => Some(client()),
        "project" => Some(project()),
        "workflow" => Some(workflow()),
        _ => None,
    }
}

/// Cross-project workflow presets (design doc §14.3).
///
/// Task fields inside are generated from `TaskInput`, the **same** shape as
/// `cgw://schema/task`, so a preset author never sees a task shape the API would reject.
fn workflow() -> Value {
    let mut value = draft07::<crate::workflow::WorkflowPreset>(
        "CompileGateway workflow preset",
        "A cross-project workflow preset (design doc §14.3): inline tasks + ordered steps + \
         parameters. Declare parameters in `variables` and write `{{name}}` in the template; \
         only **declared** variables are substituted, and other placeholders are left to the \
         build-type (manifest) layer. Applying keeps the task ids, and a same-name task or \
         batch counts as a conflict by default (overriding must be stated explicitly).",
    );

    // The embedded task definition is replaced with **the task schema itself** (minus the
    // root `$schema` / `title` and nested `definitions`), so a preset author writes tasks
    // against exactly the rules the API enforces. A test pins this word for word: once the
    // two drift, "wrote it as documented and got rejected" is guaranteed.
    let mut task_body = task();
    if let Some(object) = task_body.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
        object.remove("definitions");
    }
    if let Some(definitions) = value.get_mut("definitions").and_then(Value::as_object_mut) {
        definitions.insert("TaskInput".to_string(), task_body);
    }
    value
}

/// Tasks (design doc §7.1).
///
/// Seven kinds share one flat struct, so **which fields are required depends on `kind`**:
/// that part is expressed with `oneOf`, each branch listing the fields its kind needs.
/// Branch contents come from [`projects::KINDS`] and [`projects::required_fields`] rather
/// than a second copy (a test checks `Task::validate()` against them).
fn task() -> Value {
    let mut value = draft07::<crate::server::TaskInput>(
        "CompileGateway task",
        "A task on a project (design doc §7.1). All seven kinds share one flat structure: fill \
         in only the fields your kind uses, and the rest are ignored. See the `oneOf` branches \
         for which fields each kind requires.",
    );
    decorate_task(&mut value);
    value
}

/// Add the `kind`-dependent parts to the task-level schema: **value set + per-kind required
/// fields**.
///
/// Extracted because the task definition embedded in `cgw://schema/workflow` **must be the
/// same one**: a preset author seeing a different task shape than the API accepts is a trap
/// (and the `definitions.TaskInput` schemars generates does not know the `kind` values).
fn decorate_task(value: &mut Value) {
    if let Some(properties) = value.get_mut("properties").and_then(Value::as_object_mut) {
        if let Some(kind) = properties.get_mut("kind").and_then(Value::as_object_mut) {
            // The value set comes straight from KINDS, so a new task kind is picked up automatically
            kind.insert("enum".to_string(), json!(projects::KINDS));
        }
    }

    let branches: Vec<Value> = projects::KINDS
        .iter()
        .map(|kind| {
            json!({
                "properties": { "kind": { "const": kind } },
                "required": branch_required(kind),
            })
        })
        .collect();

    if let Some(object) = value.as_object_mut() {
        object.insert("oneOf".to_string(), Value::Array(branches));
    }
}

/// Which fields a kind's `oneOf` branch requires.
///
/// Two non-obvious points:
///
/// * **The build branch must not require `kind`.** An empty `kind` means build
///   (`TaskInput.kind` is `#[serde(default)]`), so `{"manifest": "xcode"}` is a perfectly
///   valid task -- requiring `kind` would make **every** branch reject it.
/// * Every other kind must require `kind`, since `from` / `sources` alone cannot tell the
///   branches apart (is `{"from":...,"to":...}` a copy or a move?).
fn branch_required(kind: &str) -> Vec<Value> {
    let mut required = Vec::new();
    if kind != projects::KIND_BUILD {
        required.push(Value::from("kind"));
    }
    required.extend(
        projects::required_fields(kind)
            .iter()
            .map(|field| Value::from(*field)),
    );
    required
}

/// Batch workflows (design doc §7.3).
fn batch() -> Value {
    draft07::<crate::server::BatchInput>(
        "CompileGateway batch",
        "A batch workflow on a project: run the referenced tasks in order (design doc §7.3). The \
         first failed step stops it and the later steps are marked skipped; you can retry from \
         the failed step.",
    )
}

/// Clients (`POST /api/clients`).
fn client() -> Value {
    draft07::<crate::server::ClientInput>(
        "CompileGateway client",
        "A client (target machine). An empty `id` creates one; an existing id updates that \
         record. `auth.password` / `auth.passphrase` are **write-only**: omitted means leave \
         unchanged, an empty string means clear, and they are never echoed in plain text.",
    )
}

/// Project registration (`POST /api/projects`).
fn project() -> Value {
    draft07::<crate::server::ProjectInput>(
        "CompileGateway project",
        "Register a host source directory as a project. The remote location is \
         `<client baseDir>/<project-name slug>` and does not need to be written here.",
    )
}

/// Two cleanups, both so the generated schema does not mislead.
///
/// 1. **Drop every `default` keyword.** It is a by-product of `#[serde(default)]` on the
///    struct ("what to fill in when the field is missing"), not a suggested value for users.
///    Worse, `build.steps` gets `default: []` next to `minItems: 1`, and `displayName` gets
///    `default: ""` next to `minLength: 1` -- keeping those only leads agents astray.
///    Whether a field may be omitted is already stated by `required`.
/// 2. **Unwrap `{"allOf": [{"$ref": ...}]}` back to `{"$ref": ...}`.** That wrapper exists
///    only because of the `default` above (draft-07 forbids siblings next to `$ref`, so
///    schemars wraps it in `allOf`).
pub fn tidy(node: &mut Value) {
    {
        let Value::Object(map) = node else {
            return;
        };
        map.remove("default");

        // `properties` / `definitions` are dictionaries of "name -> schema" and **are not
        // schemas themselves**, so recurse into their values only and never delete keys here
        // (that would delete the `variables[].default` field name).
        for container in ["properties", "definitions"] {
            if let Some(Value::Object(entries)) = map.get_mut(container) {
                for value in entries.values_mut() {
                    tidy(value);
                }
            }
        }
        for key in ["items", "additionalProperties", "not"] {
            if let Some(value) = map.get_mut(key) {
                tidy(value);
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::Array(items)) = map.get_mut(key) {
                for item in items.iter_mut() {
                    tidy(item);
                }
            }
        }
    }

    let only_child = node
        .get("allOf")
        .and_then(Value::as_array)
        .filter(|items| items.len() == 1)
        .map(|items| items[0].clone());
    if let Some(inner) = only_child {
        if node.as_object().map(|map| map.len()) == Some(1) {
            *node = inner;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{Location, Side};

    /// The task definition embedded in `cgw://schema/workflow` must be **word for word**
    /// identical to `cgw://schema/task` (minus the root `$schema` / `title` and nested
    /// `definitions`).
    ///
    /// A preset author writes tasks against the workflow schema while the API accepts them
    /// against the task schema; once the two drift, "wrote it as documented and got rejected"
    /// is guaranteed.
    #[test]
    fn workflow_embeds_the_very_same_task_schema() {
        let mut expected = task();
        if let Some(object) = expected.as_object_mut() {
            object.remove("$schema");
            object.remove("title");
            object.remove("definitions");
        }
        let workflow_schema = workflow();
        let embedded = workflow_schema
            .get("definitions")
            .and_then(|definitions| definitions.get("TaskInput"))
            .expect("workflow schema should define TaskInput");
        assert_eq!(
            embedded, &expected,
            "the task definition embedded in workflow no longer matches `schema task`"
        );
    }

    fn collect_refs(node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "$ref" {
                        if let Some(text) = value.as_str() {
                            out.push(text.to_string());
                        }
                    }
                    collect_refs(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_refs(item, out);
                }
            }
            _ => {}
        }
    }

    /// Look only at the **values** of a `properties` dictionary, never at its keys:
    /// `variables[].default` is a legitimate field name, not a keyword.
    fn collect_default_keywords(node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "properties" || key == "definitions" {
                        if let Some(entries) = value.as_object() {
                            for entry in entries.values() {
                                collect_default_keywords(entry, out);
                            }
                        }
                        continue;
                    }
                    if key == "default" {
                        out.push(value.to_string());
                    }
                    collect_default_keywords(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_default_keywords(item, out);
                }
            }
            _ => {}
        }
    }

    fn collect_property_names(node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "properties" {
                        if let Some(props) = value.as_object() {
                            out.extend(props.keys().cloned());
                        }
                    }
                    collect_property_names(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_property_names(item, out);
                }
            }
            _ => {}
        }
    }

    fn collect_keys(node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    out.push(key.clone());
                    collect_keys(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_keys(item, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn all_named_schemas_are_well_formed() {
        for name in NAMES {
            let schema = named(name).unwrap_or_else(|| panic!("missing schema {name}"));
            assert_eq!(
                schema["$schema"], "http://json-schema.org/draft-07/schema#",
                "{name}"
            );
            assert_eq!(schema["type"], "object", "{name}");
            assert!(schema["title"].is_string(), "{name} has no title");
            assert!(schema["description"].is_string(), "{name} has no description");

            let mut defaults = Vec::new();
            collect_default_keywords(&schema, &mut defaults);
            assert!(defaults.is_empty(), "{name} kept a `default` keyword: {defaults:?}");

            let mut refs = Vec::new();
            collect_refs(&schema, &mut refs);
            for reference in refs {
                let target = reference
                    .strip_prefix("#/definitions/")
                    .unwrap_or_else(|| panic!("{name}: unexpected $ref {reference}"));
                assert!(
                    schema["definitions"][target].is_object(),
                    "{name}: dangling $ref: {reference}"
                );
            }
        }
        assert!(named("nope").is_none());
    }

    /// The task schema `oneOf` must match [`projects::required_fields`], and the `kind`
    /// value set must follow `KINDS`.
    #[test]
    fn task_schema_branches_follow_the_kind_table() {
        let schema = named("task").unwrap();
        assert_eq!(schema["properties"]["kind"]["enum"], json!(projects::KINDS));

        let branches = schema["oneOf"].as_array().expect("task schema needs oneOf");
        assert_eq!(branches.len(), projects::KINDS.len());

        for (branch, kind) in branches.iter().zip(projects::KINDS.iter()) {
            assert_eq!(branch["properties"]["kind"]["const"], json!(kind));
            let required: Vec<&str> = branch["required"]
                .as_array()
                .expect("a branch needs required")
                .iter()
                .filter_map(Value::as_str)
                .collect();

            // Expected values are rewritten here (instead of calling branch_required), or the
            // test would only check itself
            let mut expected: Vec<&str> = Vec::new();
            if *kind != projects::KIND_BUILD {
                expected.push("kind");
            }
            expected.extend_from_slice(projects::required_fields(kind));
            assert_eq!(required, expected, "kind={kind}");
        }
        // The build branch deliberately does not require `kind`: empty means build, and that
        // document must validate too
        let build = branches
            .iter()
            .find(|branch| branch["properties"]["kind"]["const"] == json!("build"))
            .unwrap();
        assert_eq!(build["required"], json!(["manifest"]));
    }

    /// The schema properties must cover every key that appears in an instance -- the same
    /// anti-drift idea as on the manifest side.
    ///
    /// `variables` is deliberately left out: `BTreeMap` keys are user-chosen names, which
    /// show up as `additionalProperties`, not `properties`, so they cannot be compared here.
    #[test]
    fn schemas_cover_every_serialized_field() {
        let mut properties = Vec::new();
        collect_property_names(&named("task").unwrap(), &mut properties);
        let task = json!({
            "id": "t-1", "kind": "copy", "name": "n",
            "manifest": "xcode", "variables": {}, "signingMode": "unsigned",
            "artifactPath": "p", "artifacts": ["q"], "xcodePath": "/x",
            "clean": true, "from": { "side": "host", "path": "a" },
            "to": { "side": "host", "path": "b" }, "overwrite": true,
            "target": { "side": "host", "path": "a" },
            "sources": [{ "side": "host", "path": "a" }], "outputName": "o.zip",
        });
        let mut keys = Vec::new();
        collect_keys(&task, &mut keys);
        for key in keys {
            assert!(
                properties.contains(&key),
                "`{key}` from the task instance is missing from the schema: {properties:?}"
            );
        }

        properties.clear();
        collect_property_names(&named("batch").unwrap(), &mut properties);
        let batch = json!({
            "id": "b-1", "name": "n",
            "steps": [{ "taskId": "t-1", "variables": {} }],
        });
        let mut keys = Vec::new();
        collect_keys(&batch, &mut keys);
        for key in keys {
            assert!(
                properties.contains(&key),
                "`{key}` from the batch instance is missing from the schema: {properties:?}"
            );
        }
    }

    /// The client schema must list **both password-like fields**.
    ///
    /// `auth.passphrase` is exactly what the hand-written MCP-layer schema once omitted, and
    /// `ssh-keygen` encrypts private keys by default -- dropping it means "a passphrase-
    /// protected key cannot be configured over MCP". Hence this guard against the next omission.
    #[test]
    fn client_schema_covers_the_write_only_secret_fields() {
        let schema = named("client").unwrap();
        let auth = &schema["definitions"]["ClientAuthInput"];
        for field in ["method", "username", "password", "keyPath", "passphrase"] {
            assert!(auth["properties"][field].is_object(), "auth is missing {field}");
        }
        // Write-only fields: nullable, and absent means "leave unchanged"
        assert_eq!(auth["properties"]["password"]["type"], json!(["string", "null"]));
        assert_eq!(
            auth["properties"]["passphrase"]["type"],
            json!(["string", "null"])
        );
        // Only auth is required; everything else has a default (empty id = create, empty port = 22)
        assert_eq!(schema["required"], json!(["auth"]));
        assert_eq!(schema["properties"]["port"]["type"], "integer");
    }

    /// Project schema: clientId + sourcePath are required, the name is optional (last path
    /// segment).
    #[test]
    fn project_schema_requires_only_the_two_paths() {
        let schema = named("project").unwrap();
        assert_eq!(schema["required"], json!(["clientId", "sourcePath"]));
        assert_eq!(
            schema["properties"]["name"]["type"],
            json!(["string", "null"])
        );
    }

    /// `Location.side` can only be remote / host, and `from` / `to` / `target` all point at
    /// the same definition.
    #[test]
    fn location_shows_up_as_a_shared_definition() {
        let schema = named("task").unwrap();
        // A unit enum takes a definition of its own (one `const` per variant, with the variant docs)
        let variants: Vec<&str> = schema["definitions"]["Side"]["oneOf"]
            .as_array()
            .expect("Side should be oneOf")
            .iter()
            .filter_map(|variant| variant["const"].as_str())
            .collect();
        assert_eq!(variants, vec!["remote", "host"]);

        // `side` carries docs, so schemars wraps it in allOf
        // (draft-07 forbids siblings next to `$ref`, so the description sits outside)
        let location = &schema["definitions"]["Location"];
        assert_eq!(
            location["properties"]["side"]["allOf"][0]["$ref"],
            "#/definitions/Side"
        );
        for field in ["from", "to", "target"] {
            assert_eq!(
                schema["properties"][field]["allOf"][0]["$ref"], "#/definitions/Location",
                "{field} should reference the shared Location definition"
            );
        }
        assert_eq!(
            schema["properties"]["sources"]["items"]["$ref"], "#/definitions/Location"
        );
    }

    /// batch: steps must be non-empty and each step's taskId must be non-empty -- all three
    /// of CLI, WebUI and the API now agree (the API used to **silently drop** an empty taskId).
    #[test]
    fn batch_schema_states_the_step_requirements() {
        let schema = named("batch").unwrap();
        assert_eq!(schema["required"], json!(["steps"]));
        let steps = &schema["properties"]["steps"];
        assert_eq!(steps["type"], "array");
        assert_eq!(steps["minItems"], 1);
        assert_eq!(steps["items"]["$ref"], "#/definitions/BatchStep");

        let step = &schema["definitions"]["BatchStep"];
        assert_eq!(step["required"], json!(["taskId"]));
        assert_eq!(step["properties"]["taskId"]["minLength"], 1);
    }

    /// Consistency between task / batch and the implementation: build the **minimal** JSON
    /// for each kind from the schema's `required`, `Task::validate()` must accept it, and
    /// dropping any required field must produce an error.
    ///
    /// This pins "what the schema says" to "what the code does": changing one side alone
    /// breaks it.
    #[test]
    fn task_schema_required_matches_validate() {
        fn minimal(kind: &str) -> serde_json::Map<String, Value> {
            let mut map = serde_json::Map::new();
            map.insert("kind".to_string(), json!(kind));
            map.insert("name".to_string(), json!("test"));
            for field in projects::required_fields(kind) {
                let value = match *field {
                    "manifest" => json!("xcode"),
                    "from" | "to" | "target" => {
                        json!({ "side": "host", "path": format!("dir-{field}") })
                    }
                    "sources" => json!([{ "side": "host", "path": "src" }]),
                    other => panic!("the test does not know required field {other}; add it"),
                };
                map.insert((*field).to_string(), value);
            }
            map
        }

        for kind in projects::KINDS {
            let full = Value::Object(minimal(kind));
            let input: crate::server::TaskInput = serde_json::from_value(full)
                .unwrap_or_else(|err| panic!("{kind}: failed to parse the minimal JSON: {err}"));
            let errors = input.into_task("t-test".to_string()).validate();
            assert!(
                errors.is_empty(),
                "{kind}: a task built from the schema's required fields should have no errors: {errors:?}"
            );

            // Drop the required fields one by one; validate must notice
            for field in projects::required_fields(kind) {
                let mut broken = minimal(kind);
                broken.remove(*field);
                let input: crate::server::TaskInput =
                    serde_json::from_value(Value::Object(broken))
                        .unwrap_or_else(|err| panic!("{kind}: failed to parse after removing {field}: {err}"));
                let errors = input.into_task("t-test".to_string()).validate();
                assert!(
                    !errors.is_empty(),
                    "{kind}: required field {field} is missing; the schema rejects it and validate() must too"
                );
            }
        }

        // An empty `kind` means build -- the schema's build branch deliberately omits `kind`,
        // and the implementation must mean the same (dropping kind still leaves a valid task).
        let mut no_kind = minimal(projects::KIND_BUILD);
        no_kind.remove("kind");
        let input: crate::server::TaskInput = serde_json::from_value(Value::Object(no_kind)).unwrap();
        assert!(input.into_task("t-test".to_string()).validate().is_empty());
    }

    /// Also confirm the `Location` default (a missing `side` in the API means remote).
    #[test]
    fn location_defaults_to_remote() {
        assert_eq!(Side::default(), Side::Remote);
        let location: Location = serde_json::from_str(r#"{"path":"a"}"#).unwrap();
        assert_eq!(location.side, Side::Remote);
    }
}
