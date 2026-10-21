#!/usr/bin/env node
/**
 * Check the JSON Schemas the daemon generates with a real validator (ajv) (design doc section 2).
 *
 * Cases on the Rust side can only assert "which fields/constraints the schema has", which does
 * not prove it is a draft-07 schema other tools can actually consume. The design doc states the
 * purpose clearly: let an AI agent validate a manifest itself before editing it - so verify it
 * once from the agent's side.
 *
 * For each of the six schemas (manifest / task / batch / client / project / workflow):
 *   1. it compiles under ajv (valid draft-07 keywords, all $refs resolve);
 *   2. documents that really exist in the daemon pass it (manifest files and project tasks);
 *   3. negative cases must be rejected and must hit the expected keyword;
 *   4. positive samples (full / minimal / with unknown fields) must be accepted.
 *
 * Point 4's "with unknown fields" is deliberate: a schema must not be stricter than the
 * implementation. serde ignores unknown fields, so the schema accepts them too - a schema that
 * disagrees with the implementation is worse than no schema.
 *
 * Usage (needs a running daemon):
 *   CGW_URL=http://localhost:65251 node test/schema.mjs
 *   node test/schema.mjs --url http://127.0.0.1:3993
 *   node test/schema.mjs --schema /tmp/manifest.schema.json   # offline, manifest only
 */

import { readFileSync } from "node:fs";
import Ajv from "ajv";

const args = process.argv.slice(2);
function option(name, fallback) {
  const index = args.indexOf(name);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
}

const url = option("--url", process.env.CGW_URL ?? "");
const schemaPath = option("--schema", "");
const username = process.env.CGW_USERNAME ?? "";
const password = process.env.CGW_PASSWORD ?? "";

let failures = 0;
function check(ok, what, detail = "") {
  if (ok) {
    console.log(`  ok   ${what}`);
  } else {
    console.log(`  FAIL ${what}${detail ? `\n       ${detail}` : ""}`);
    failures += 1;
  }
}

/** Small HTTP client for the daemon (keeps the cookie and reuses it after login). */
function client(base) {
  let cookie = "";
  return async function call(method, path, body) {
    const headers = { "X-Client": "webui" };
    if (cookie) headers.Cookie = cookie;
    if (body !== undefined) headers["Content-Type"] = "application/json";
    const response = await fetch(`${base}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const setCookie = response.headers.getSetCookie?.() ?? [];
    if (setCookie.length) cookie = setCookie.map((c) => c.split(";")[0]).join("; ");
    const text = await response.text();
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = text;
    }
    return { status: response.status, body: parsed };
  };
}

/** Load schemas: with `--schema` validate manifest offline only, otherwise fetch all five from the daemon. */
async function loadSchemas() {
  if (schemaPath) {
    console.log("(offline mode: validating only the file from --schema, treated as manifest)\n");
    return { schemas: { manifest: JSON.parse(readFileSync(schemaPath, "utf8")) }, docs: {} };
  }
  if (!url) {
    throw new Error("need --schema <file> or --url <daemon> (or set CGW_URL)");
  }

  const call = client(url);
  if (username) {
    const login = await call("POST", "/api/login", { username, password });
    if (login.status !== 200) {
      throw new Error(`login failed (${login.status}): ${JSON.stringify(login.body)}`);
    }
  }

  const schemas = {};
  for (const name of ["manifest", "task", "batch", "client", "project", "workflow"]) {
    const got = await call("GET", `/api/schema/${name}`);
    if (got.status !== 200) {
      throw new Error(`failed to fetch ${name} schema (${got.status}): ${JSON.stringify(got.body)}`);
    }
    schemas[name] = got.body;
  }
  // the legacy path must stay alive too (calls already written into scripts/docs must not break)
  const legacy = await call("GET", "/api/schema/manifest");
  if (legacy.status !== 200) {
    throw new Error(`/api/schema/manifest is broken (${legacy.status})`);
  }

  // real documents: manifests on disk + workflow templates + tasks on projects
  const docs = { manifests: [], tasks: [], workflows: [] };
  const list = await call("GET", "/api/manifests");
  for (const summary of list.body.manifests ?? []) {
    const raw = await call("GET", `/api/manifests/${encodeURIComponent(summary.id)}/raw`);
    docs.manifests.push({ id: summary.id, doc: JSON.parse(raw.body.raw) });
  }
  const workflows = await call("GET", "/api/workflows");
  for (const summary of workflows.body.workflows ?? []) {
    const raw = await call("GET", `/api/workflows/${encodeURIComponent(summary.id)}/raw`);
    docs.workflows.push({ id: summary.id, doc: JSON.parse(raw.body.parsed ? raw.body.raw : "{}") });
  }
  const projects = await call("GET", "/api/projects");
  for (const project of projects.body.projects ?? []) {
    for (const task of project.tasks ?? []) {
      docs.tasks.push({ id: `${project.name}/${task.name || task.id}`, doc: task });
    }
  }
  return { schemas, docs };
}

const { schemas, docs } = await loadSchemas();
const validators = {};

console.log("--- 1. ajv compiles these schemas ---");
for (const [name, schema] of Object.entries(schemas)) {
  const ajv = new Ajv({ allErrors: true, strict: false });
  try {
    validators[name] = ajv.compile(schema);
    check(
      true,
      `${name}: draft-07 compiled (${Object.keys(schema.properties ?? {}).length} fields)` +
        (schema.oneOf ? `, ${schema.oneOf.length} oneOf branches` : ""),
    );
  } catch (err) {
    check(false, `${name}: compilation failed`, String(err));
  }
}
if (failures) {
  console.log("\nschema compilation failed, the remaining cases cannot run");
  process.exit(1);
}

const bad = (name, sample, keyword) => {
  const validate = validators[name];
  const ok = validate(sample);
  const hit = ok ? false : (validate.errors ?? []).some((e) => e.keyword === keyword);
  return { ok, hit, errors: JSON.stringify(validate.errors) };
};

function expectRejected(name, what, sample, keyword) {
  const { ok, hit, errors } = bad(name, sample, keyword);
  check(!ok && hit, `${name}: ${what} was rejected (hit ${keyword})`, errors);
}
function expectAccepted(name, what, sample) {
  const validate = validators[name];
  const ok = validate(sample);
  check(ok, `${name}: ${what} was accepted`, JSON.stringify(validate.errors));
}

if (docs.manifests?.length) {
  console.log("--- 2a. manifests on disk must pass ---");
  for (const { id, doc } of docs.manifests) {
    const ok = validators.manifest(doc);
    check(ok, `${id} passes the manifest schema`, JSON.stringify(validators.manifest.errors));
  }
}
if (docs.tasks?.length) {
  console.log("--- 2b. real tasks in the daemon must pass ---");
  for (const { id, doc } of docs.tasks) {
    const ok = validators.task(doc);
    check(ok, `${id} (${doc.kind}) passes the task schema`, JSON.stringify(validators.task.errors));
  }
}
if (docs.workflows?.length) {
  console.log("--- 2c. workflow templates on disk must pass (inline tasks included) ---");
  for (const { id, doc } of docs.workflows) {
    const ok = validators.workflow(doc);
    check(ok, `${id} passes the workflow schema`, JSON.stringify(validators.workflow.errors));
  }
}

console.log("--- 3. negative cases must be rejected ---");
const minimalManifest = { displayName: "T", build: { steps: ["echo hi"] } };
expectRejected("manifest", "missing displayName", { build: { steps: ["x"] } }, "required");
expectRejected("manifest", "missing build", { displayName: "T" }, "required");
expectRejected("manifest", "build.steps empty array", { displayName: "T", build: { steps: [] } }, "minItems");
expectRejected("manifest", "displayName empty string", { displayName: "", build: { steps: ["x"] } }, "minLength");
expectRejected("manifest", "steps is not an array", { displayName: "T", build: { steps: "echo" } }, "type");
// the two ways to write a build step (design doc section 7.6.9): a string, or `{id, run}`
expectAccepted("manifest", "step with a name", {
  displayName: "T",
  build: { steps: [{ id: "link", run: "echo hi" }] },
});
expectAccepted("manifest", "both forms mixed", {
  displayName: "T",
  build: { steps: ["echo one", { id: "link", run: "echo two" }] },
});
expectRejected(
  "manifest",
  "named step missing run",
  { displayName: "T", build: { steps: [{ id: "link" }] } },
  "anyOf",
);
expectRejected(
  "manifest",
  "named step with empty id",
  { displayName: "T", build: { steps: [{ id: "", run: "echo hi" }] } },
  "anyOf",
);
expectRejected(
  "manifest",
  "variable without name",
  { displayName: "T", build: { steps: ["x"] }, variables: [{ label: "S" }] },
  "required",
);

const copyTask = { kind: "copy", from: { path: "a" }, to: { path: "b" } };
expectRejected("task", "unknown kind", { kind: "nope" }, "enum");
expectRejected("task", "copy missing to", { kind: "copy", from: { path: "a" } }, "required");
// workflow template (design doc section 14.3): inline tasks use the very same task definition
expectRejected("workflow", "missing tasks", { name: "W", steps: [] }, "required");
expectRejected("workflow", "missing steps", { name: "W", tasks: [] }, "required");
expectRejected("workflow", "name empty string", { name: "", tasks: [], steps: [] }, "minLength");
expectRejected(
  "workflow",
  "inline task with an unknown kind",
  { name: "W", tasks: [{ kind: "nope" }], steps: [{ taskId: "t" }] },
  "enum",
);
expectRejected("task", "build missing manifest", { kind: "build" }, "required");
expectRejected("task", "pull missing sources", { kind: "pull", to: { path: "b" } }, "required");
expectRejected("task", "delete missing target", { kind: "delete" }, "required");
expectRejected("task", "manifest empty string", { kind: "build", manifest: "" }, "minLength");
expectRejected("task", "wrong side", { ...copyTask, from: { side: "both", path: "a" } }, "oneOf");

expectRejected("batch", "steps empty array", { name: "b", steps: [] }, "minItems");
expectRejected("batch", "missing steps", { name: "b" }, "required");
expectRejected("batch", "step without taskId", { steps: [{ variables: {} }] }, "required");
expectRejected("batch", "step with empty taskId", { steps: [{ taskId: "" }] }, "minLength");

expectRejected("client", "missing auth", { name: "c", host: "1.2.3.4" }, "required");
expectRejected("client", "port written as a string", { host: "h", auth: {}, port: "22" }, "type");
expectRejected("project", "missing sourcePath", { clientId: "c-1" }, "required");
expectRejected("project", "clientId has the wrong type", { clientId: 7, sourcePath: "/s" }, "type");

console.log("--- 4. positive samples must pass ---");
expectAccepted("manifest", "full manifest", {
  displayName: "Xcode project",
  platform: "xcode",
  include: { paths: ["ios/*.xcodeproj"] },
  variables: [{ name: "scheme", label: "Scheme", required: true, default: "Runner" }],
  build: {
    steps: ["xcodebuild -scheme {{scheme}} {{signingExtraArgs}}"],
    artifactPath: "out/{{scheme}}.app",
    artifacts: ["out/{{scheme}}.app"],
  },
  cache: { paths: [".cgw/derived-data"] },
});
expectAccepted("manifest", "required fields only", minimalManifest);
expectAccepted("manifest", "with unknown fields (matches serde's leniency)", {
  ...minimalManifest,
  typoField: 1,
});

expectAccepted("task", "copy task", copyTask);
expectAccepted("task", "build task", { kind: "build", manifest: "xcode", variables: { a: "b" } });
expectAccepted("task", "push task (needs nothing filled in)", { kind: "push" });
expectAccepted("task", "kind omitted (means build) + manifest", { manifest: "xcode" });
expectAccepted("task", "carrying fields of another kind (the implementation ignores them)", {
  kind: "delete",
  target: { path: "a" },
  from: { path: "x" },
  clean: true,
});

expectAccepted("batch", "normal batch", {
  name: "full pipeline",
  steps: [{ taskId: "t-1" }, { taskId: "t-2", variables: { scheme: "Runner" } }],
});
expectAccepted("batch", "name omitted (defaults to batch)", { steps: [{ taskId: "t-1" }] });
expectAccepted("batch", "with unknown fields", { steps: [{ taskId: "t-1", extra: 1 }], extra: 2 });

expectAccepted("client", "minimal client (auth only)", { auth: {} });
expectAccepted("client", "password login", {
  name: "phone",
  host: "192.168.1.9",
  port: 22,
  auth: { method: "password", username: "u", password: "p" },
  tags: ["ios"],
  baseDir: "~/CompileGateway",
});
expectAccepted("client", "private key + passphrase (write-only field can be null)", {
  name: "mac",
  host: "mac.local",
  auth: { method: "key", username: "u", keyPath: "/k", passphrase: null },
});
expectAccepted("project", "normal registration", {
  clientId: "c-1",
  sourcePath: "/srv/src",
  name: "demo",
});
expectAccepted("project", "name is optional", { clientId: "c-1", sourcePath: "/srv/src" });

if (failures) {
  console.log(`\n${failures} checks did not pass`);
  process.exit(1);
}
console.log("\nall passed");


