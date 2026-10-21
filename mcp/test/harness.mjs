#!/usr/bin/env node
/**
 * Protocol-level self-test for the MCP adapter.
 *
 * This machine has no Claude Code / Codex installed, so the test feeds JSON-RPC straight
 * into the MCP stdio transport: `initialize` -> `tools/list` -> a series of `tools/call`.
 * That verifies the protocol layer (handshake, capability declaration, tool schema,
 * result/error wrapping) over the same path a real client takes.
 *
 * Usage:
 *   node test/harness.mjs              # run everything
 *   node test/harness.mjs --list       # just print the tool list
 */

import Ajv from "ajv";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const serverPath = resolve(here, "../dist/index.js");
const onlyList = process.argv.includes("--list");

/** A minimal JSON-RPC over stdio client (waits for the response matching an id). */
class StdioClient {
  constructor(child) {
    this.child = child;
    this.buffer = "";
    this.pending = new Map();
    this.nextId = 1;
    this.stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => this.onData(chunk));
    child.stderr.on("data", (chunk) => {
      this.stderr += chunk;
    });
    child.on("exit", (code) => {
      for (const [, reject] of this.pending) {
        reject(new Error(`MCP server exited (code=${code})\n${this.stderr}`));
      }
      this.pending.clear();
    });
  }

  onData(chunk) {
    this.buffer += chunk;
    // the MCP stdio transport is "one JSON object per line"
    let index;
    while ((index = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, index).trim();
      this.buffer = this.buffer.slice(index + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        throw new Error(`server wrote a non-JSON line (stdout is polluted): ${line}`);
      }
      if (message.id !== undefined && this.pending.has(message.id)) {
        const { resolve: done, reject } = this.pending.get(message.id);
        this.pending.delete(message.id);
        if (message.error) {
          reject(new Error(JSON.stringify(message.error)));
        } else {
          done(message.result);
        }
      }
    }
  }

  request(method, params) {
    const id = this.nextId++;
    const payload = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
    return new Promise((done, reject) => {
      this.pending.set(id, { resolve: done, reject });
      this.child.stdin.write(payload);
    });
  }

  notify(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
  }
}

async function main() {
  const child = spawn(process.execPath, [serverPath], { stdio: ["pipe", "pipe", "pipe"] });
  const client = new StdioClient(child);

  const init = await client.request("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "harness", version: "1.0.0" },
  });
  console.log(`handshake ok: ${init.serverInfo.name} ${init.serverInfo.version}`);
  console.log(`capabilities: ${Object.keys(init.capabilities ?? {}).join(", ")}`);
  client.notify("notifications/initialized", {});

  const { tools } = await client.request("tools/list", {});
  console.log(`tools: ${tools.length}`);
  for (const tool of tools) {
    if (onlyList) {
      console.log(`  ${tool.name.padEnd(24)} ${tool.description.slice(0, 60)}`);
    }
  }
  if (onlyList) {
    child.kill();
    return;
  }

  // ---- endpoint discovery: the bridge must also honor the runtime.json in `CGW_HOME`
  // (same resolution chain as `cgw`) ----
  //
  // Create a temp `CGW_HOME` holding a runtime.json that points at a dead port, then
  // start the bridge from another empty dir (so it cannot have guessed the endpoint from
  // cwd). The bridge prints the resolved endpoint to stderr; assert that line carries the
  // dead port. That pins this source without knowing the real endpoint, and never touches
  // a real daemon.
  {
    const homeDir = await mkdtemp(join(tmpdir(), "cgw-bridge-home-"));
    const cwdDir = await mkdtemp(join(tmpdir(), "cgw-bridge-cwd-"));
    await writeFile(
      join(homeDir, "runtime.json"),
      JSON.stringify({
        url: "http://127.0.0.1:1",
        mcpUrl: "http://127.0.0.1:1",
        mcpPort: 1,
        mcpEnabled: true,
      }),
    );
    const probe = spawn(process.execPath, [serverPath], {
      stdio: ["pipe", "pipe", "pipe"],
      cwd: cwdDir,
      env: { ...process.env, CGW_HOME: homeDir, CGW_MCP_URL: "" },
    });
    const reported = await new Promise((done) => {
      let text = "";
      let settled = false;
      const finish = () => {
        if (!settled) {
          settled = true;
          done(text);
        }
      };
      probe.stderr.setEncoding("utf8");
      probe.stderr.on("data", (chunk) => {
        text += chunk;
        if (text.includes("http://127.0.0.1:1/mcp")) {
          probe.kill();
          finish();
        }
      });
      probe.on("exit", finish);
      probe.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} })}\n`,
      );
      setTimeout(() => {
        probe.kill();
        finish();
      }, 5000);
    });
    await rm(homeDir, { recursive: true, force: true });
    await rm(cwdDir, { recursive: true, force: true });
    if (!reported.includes("http://127.0.0.1:1/mcp")) {
      throw new Error(`bridge did not discover the endpoint from CGW_HOME: ${JSON.stringify(reported.trim())}`);
    }
    console.log("endpoint discovery: runtime.json in CGW_HOME was honored ✓");
  }

  // every tool's inputSchema must be a valid object schema: clients build argument forms from it
  const bad = tools.filter(
    (tool) =>
      tool.inputSchema?.type !== "object" ||
      typeof tool.inputSchema.properties !== "object" ||
      typeof tool.name !== "string" ||
      !tool.description,
  );
  if (bad.length) {
    throw new Error(`${bad.length} tools have an invalid schema: ${bad.map((t) => t.name).join(", ")}`);
  }
  console.log("every tool's inputSchema is a valid object schema ✓");

  // ---- resources: the five JSON Schemas (design doc section 2, generated by the daemon from Rust structs) ----
  const { resources } = await client.request("resources/list", {});
  const wanted = ["manifest", "task", "batch", "client", "project"];
  for (const name of wanted) {
    const ref = resources.find((item) => item.uri === `cgw://schema/${name}`);
    if (!ref) {
      throw new Error(`resources/list has no ${name} schema: ${JSON.stringify(resources)}`);
    }
    const read = await client.request("resources/read", { uri: ref.uri });
    const schema = JSON.parse(read.contents?.[0]?.text ?? "");
    if (schema.$schema !== "http://json-schema.org/draft-07/schema#") {
      throw new Error(`${name} schema has the wrong $schema: ${schema.$schema}`);
    }
    if (!schema.properties || !schema.title) {
      throw new Error(`${name} schema has the wrong structure`);
    }
    console.log(
      `resources/read ${ref.uri} -> ${Object.keys(schema.properties).length} fields` +
        (schema.required ? `,required=[${schema.required.join(", ")}]` : "") +
        (schema.oneOf ? `,oneOf=${schema.oneOf.length} branches` : "") +
        " ✓",
    );
  }

  // the task schema's oneOf is the authoritative "which fields each kind requires" spec; cross-check it
  const taskRef = resources.find((item) => item.uri === "cgw://schema/task");
  const taskSchema = JSON.parse(
    (await client.request("resources/read", { uri: taskRef.uri })).contents[0].text,
  );
  const kinds = taskSchema.properties.kind.enum;
  if (kinds.length !== taskSchema.oneOf.length) {
    throw new Error("task schema's oneOf branch count does not match the kind enum size");
  }

  // ---- tool schemas must match the one the daemon generates ----
  //
  // Tool arguments are no longer hand-written in TS (that tool table was deleted, see the
  // header of mcp/src/index.ts): what `tools/list` embeds is the schema the daemon itself
  // generates. Comparing field by field guards against drift such as a bad copy or a
  // dropped field. It really happened before: `cgw_save_client` was missing
  // `auth.passphrase`, so "private key with passphrase" could not be configured over MCP
  // (ssh-keygen encrypts private keys by default).
  const schemaOf = {};
  for (const name of wanted) {
    const ref = resources.find((item) => item.uri === `cgw://schema/${name}`);
    schemaOf[name] = JSON.parse(
      (await client.request("resources/read", { uri: ref.uri })).contents[0].text,
    );
  }

  /** Resolve `$ref` into definitions. */
  const deref = (schema, node) => {
    if (node && typeof node.$ref === "string") {
      const target = node.$ref.replace(/^#\/definitions\//, "");
      return schema.definitions?.[target] ?? node;
    }
    // an allOf with a single $ref is equivalent to it (fields with a description get wrapped)
    if (node && Array.isArray(node.allOf) && node.allOf.length === 1) {
      return deref(schema, node.allOf[0]);
    }
    return node;
  };

  /** Only compares "how many properties a level has"; types are compared loosely (integer = number, nullable = non-nullable). */
  const compareProperties = (
    where,
    daemonSchema,
    daemonNode,
    toolSchema,
    toolNode,
    allowedExtras = [],
  ) => {
    const daemonProps = Object.keys(deref(daemonSchema, daemonNode)?.properties ?? {}).sort();
    const toolProps = Object.keys(toolNode?.properties ?? {}).sort();
    // a missing field is always drift; extra fields are only allowed as "routing fields"
    // (such as projectId, which goes into the URL rather than the body) and must be listed
    // explicitly one by one, never waived in bulk.
    const missing = daemonProps.filter((key) => !toolProps.includes(key));
    const extra = toolProps.filter(
      (key) => !daemonProps.includes(key) && !allowedExtras.includes(key),
    );
    if (missing.length || extra.length) {
      throw new Error(
        `${where}: tool schema does not match the daemon schema. ` +
          `tool is missing [${missing.join(", ")}], has undeclared extras [${extra.join(", ")}]`,
      );
    }
    for (const key of daemonProps) {
      const daemonType = deref(daemonSchema, deref(daemonSchema, daemonNode).properties[key]);
      const toolType = toolNode.properties[key];
      if (!daemonType?.type || !toolType?.type) continue;
      const norm = (value) => {
        const list = Array.isArray(value) ? value.filter((item) => item !== "null") : [value];
        return list.map((item) => (item === "integer" ? "number" : item)).sort().join("|");
      };
      if (norm(daemonType.type) !== norm(toolType.type)) {
        throw new Error(
          `${where}.${key}: type mismatch (daemon=${JSON.stringify(daemonType.type)} ` +
            `tool=${JSON.stringify(toolType.type)})`,
        );
      }
    }
    return daemonProps;
  };

  const toolOf = (name) => tools.find((tool) => tool.name === name);
  const pairs = [
    ["cgw_save_task", "task", (tool) => tool.inputSchema.properties.task, []],
    ["cgw_save_batch", "batch", (tool) => tool.inputSchema, ["projectId"]],
    ["cgw_save_client", "client", (tool) => tool.inputSchema, []],
    ["cgw_create_project", "project", (tool) => tool.inputSchema, []],
  ];
  let compared = 0;
  for (const [toolName, schemaName, pick, extras] of pairs) {
    const tool = toolOf(toolName);
    if (!tool) throw new Error(`no ${toolName} in the tool table`);
    const daemonSchema = schemaOf[schemaName];
    const props = compareProperties(
      `${toolName} vs /api/schema/${schemaName}`,
      daemonSchema,
      daemonSchema,
      tool.inputSchema,
      pick(tool),
      extras,
    );
    compared += props.length;
    console.log(`  ${toolName.padEnd(20)} ≡ /api/schema/${schemaName} (${props.length} fields) ✓`);
  }
  // the two nested levels must match too: a client's auth, a task's from/to (Location), a batch's steps
  const saveClient = toolOf("cgw_save_client").inputSchema;
  compareProperties(
    "cgw_save_client.auth",
    schemaOf.client,
    schemaOf.client.properties.auth,
    saveClient,
    saveClient.properties.auth,
  );
  compareProperties(
    "cgw_save_task.task.from",
    taskSchema,
    taskSchema.properties.from,
    toolOf("cgw_save_task").inputSchema.properties.task,
    toolOf("cgw_save_task").inputSchema.properties.task.properties.from,
  );
  const batchSchema = schemaOf.batch;
  compareProperties(
    "cgw_save_batch.steps.items",
    batchSchema,
    batchSchema.properties.steps.items,
    toolOf("cgw_save_batch").inputSchema,
    toolOf("cgw_save_batch").inputSchema.properties.steps.items,
  );

  // The task schema's `oneOf` (which fields each kind requires) must match on both sides.
  // This statement used to be hand-written in the tool description, where nothing could
  // stop it from drifting; now it is structure, compared branch by branch here.
  const daemonBranches = new Map(
    (taskSchema.oneOf ?? []).map((branch) => [
      branch.properties.kind.const,
      [...(branch.required ?? [])].sort().join(","),
    ]),
  );
  const toolTaskSchema = toolOf("cgw_save_task").inputSchema.properties.task;
  const toolBranches = new Map(
    (toolTaskSchema.oneOf ?? []).map((branch) => [
      branch.properties.kind.const,
      [...(branch.required ?? [])].sort().join(","),
    ]),
  );
  if (daemonBranches.size === 0 || toolBranches.size === 0) {
    throw new Error("task oneOf branches are gone (daemon or tool side)");
  }
  for (const [kind, required] of daemonBranches) {
    if (!toolBranches.has(kind)) {
      throw new Error(`tool task schema is missing the oneOf branch for ${kind}`);
    }
    if (toolBranches.get(kind) !== required) {
      throw new Error(
        `${kind} required fields differ: daemon=[${required}] tool=[${toolBranches.get(kind)}]`,
      );
    }
  }
  for (const kind of toolBranches.keys()) {
    if (!daemonBranches.has(kind)) {
      throw new Error(`tool task schema has a kind the daemon does not: ${kind}`);
    }
  }
  console.log(
    `  cgw_save_task.oneOf   ≡ /api/schema/task (required fields identical across all ${daemonBranches.size} branches) ✓`,
  );

  // "same structure on both sides" is not enough - that `oneOf` must be a working JSON
  // Schema so client validators actually block bad input. Run positive and negative cases through ajv.
  {
    const ajv = new Ajv({ allErrors: true, strict: false });
    const validateTask = ajv.compile(toolTaskSchema);
    const cases = [
      ["copy missing to", { kind: "copy", from: { side: "host", path: "a" } }, false],
      ["copy complete", { kind: "copy", from: { side: "host", path: "a" }, to: { side: "host", path: "b" } }, true],
      ["build without kind (means build)", { manifest: "xcode" }, true],
      ["build missing manifest", { kind: "build" }, false],
      ["push needs nothing", { kind: "push" }, true],
      ["export missing sources", { kind: "export" }, false],
      ["unknown kind", { kind: "nope" }, false],
    ];
    for (const [what, sample, expected] of cases) {
      const ok = validateTask(sample);
      if (ok !== expected) {
        throw new Error(
          `tool schema oneOf did not behave as expected: ${what} expected ${expected} got ${ok} ` +
            JSON.stringify(validateTask.errors),
        );
      }
    }
    console.log(`  cgw_save_task.oneOf   ${cases.length} positive/negative cases passed / rejected as expected ✓`);
  }

  console.log(`tool schemas match the daemon schemas (${compared} fields + three nested levels + oneOf) ✓`);

  const call = async (name, args = {}) => {
    const result = await client.request("tools/call", { name, arguments: args });
    return result;
  };
  const text = (result) => result.content.map((part) => part.text ?? "").join("\n");

  // ---- read-only calls ----
  const status = await call("cgw_status");
  const statusBody = JSON.parse(text(status));
  console.log(`\ncgw_status -> ${statusBody.app} ${statusBody.version} @ ${statusBody.url}`);

  const clients = JSON.parse(text(await call("cgw_list_clients")));
  console.log(`cgw_list_clients -> ${clients.clients?.length ?? 0} clients`);

  const projects = JSON.parse(text(await call("cgw_list_projects")));
  console.log(`cgw_list_projects -> ${projects.projects?.length ?? 0} projects`);

  const manifests = JSON.parse(text(await call("cgw_list_manifests")));
  console.log(`cgw_list_manifests -> ${manifests.manifests?.length ?? 0} manifests`);

  const browse = JSON.parse(text(await call("cgw_browse", {})));
  console.log(`cgw_browse -> root ${browse.root}, ${browse.entries?.length ?? 0} subdirectories`);

  const doctor = JSON.parse(text(await call("cgw_status")));
  console.log(`cgw_status is readable ✓ (${doctor.configPath})`);

  // ---- one write plus one end-to-end job run ----
  const home = process.env.USERPROFILE ?? process.env.HOME;
  const { mkdirSync, writeFileSync } = await import("node:fs");
  const sourceDir = resolve(home, "cgw-mcp-proj");
  mkdirSync(sourceDir, { recursive: true });
  writeFileSync(resolve(sourceDir, "hello.txt"), "from mcp");

  const host = process.env.CGW_TEST_HOST ?? "127.0.0.1";
  const clientInfo = JSON.parse(
    text(
      await call("cgw_save_client", {
        name: "mcp-test",
        host,
        port: 22,
        auth: { method: "password", username: "x", password: "y" },
      }),
    ),
  );
  const clientId = clientInfo.client.id;
  console.log(`\ncgw_save_client -> ${clientId}`);

  const project = JSON.parse(
    text(await call("cgw_create_project", { clientId, sourcePath: sourceDir })),
  );
  const projectId = project.project.id;
  console.log(`cgw_create_project -> ${projectId}`);

  // the root of side=host is the daemon's managed dir (~/CompileGateway/<slug>)
  const managed = resolve(home, "CompileGateway", "cgw-mcp-proj");
  mkdirSync(resolve(managed, "src"), { recursive: true });
  writeFileSync(resolve(managed, "src", "a.txt"), "payload");

  const saved = JSON.parse(
    text(
      await call("cgw_save_task", {
        projectId,
        task: {
          kind: "copy",
          name: "mcp-copy",
          from: { side: "host", path: "src" },
          to: { side: "host", path: "out" },
          overwrite: true,
        },
      }),
    ),
  );
  const tasks = JSON.parse(text(await call("cgw_list_tasks", { projectId })));
  // find the one just created by id - do not use projects[0], the daemon may hold others
  const mine = tasks.projects.find((item) => item.id === projectId);
  if (!mine?.tasks?.length) {
    throw new Error(`no saved task visible in project ${projectId}: ${JSON.stringify(tasks).slice(0, 300)}`);
  }
  const taskId = mine.tasks.at(-1).id;
  console.log(`cgw_save_task -> ${taskId}`);

  const preview = JSON.parse(text(await call("cgw_preview_task", { projectId, taskId })));
  console.log(`cgw_tasks_preview -> kind=${preview.kind} from=${JSON.stringify(preview.from)}`);

  const run = JSON.parse(text(await call("cgw_run_task", { projectId, taskId })));
  console.log(`cgw_run_task -> ${run.jobId}`);

  // follow the job to completion
  let job = null;
  for (let attempt = 0; attempt < 30; attempt++) {
    await new Promise((r) => setTimeout(r, 300));
    job = JSON.parse(text(await call("cgw_job", { jobId: run.jobId })));
    if (job.state !== "running") break;
  }
  console.log(`cgw_job -> ${job.state} — ${job.message}`);

  const tree = JSON.parse(text(await call("cgw_tree", { projectId, side: "host", path: "out" })));
  console.log(
    `cgw_tree -> ${tree.tree.entries.map((e) => `${e.name}(${e.size}B)`).join(", ")}`,
  );

  // ---- error wrapping: a missing job must be isError and carry structured info ----
  const missing = await call("cgw_job", { jobId: "j-nope" });
  console.log(`\nerror wrapping -> isError=${missing.isError} text=${text(missing).split("\n")[0]}`);
  if (!missing.isError) {
    throw new Error("a missing job should return isError");
  }

  // ---- cleanup ----
  await call("cgw_delete_task", { projectId, taskId });
  await call("cgw_delete_project", { id: projectId });
  await call("cgw_delete_client", { id: clientId });
  const { rmSync } = await import("node:fs");
  rmSync(sourceDir, { recursive: true, force: true });
  rmSync(managed, { recursive: true, force: true });
  console.log("\ncleanup done. all passed.");

  child.kill();
}

main().catch((error) => {
  console.error(`\nfailed: ${error.message}`);
  process.exit(1);
});

