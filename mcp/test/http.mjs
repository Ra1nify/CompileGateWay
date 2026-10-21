#!/usr/bin/env node
/**
 * Self-test for the **Streamable HTTP transport** of the daemon's built-in MCP endpoint.
 *
 * The MCP body now runs inside the daemon (`src-tauri/src/mcp.rs`), listening on the
 * daemon's **second port** at the endpoint `POST /mcp`. This script sends
 * `initialize` / `tools/list` / `resources/list` / `tools/call` per the spec, then pokes
 * `/healthz` and a few edge cases. **No MCP client is needed.**
 *
 * Usage:
 *   node test/http.mjs                                   # find via runtime.json / CGW_MCP_URL
 *   node test/http.mjs http://127.0.0.1:3994/mcp         # explicit endpoint
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));

let failures = 0;
function check(ok, what, detail = "") {
  if (ok) {
    console.log(`  ok   ${what}`);
  } else {
    console.log(`  FAIL ${what}${detail ? `\n       ${detail}` : ""}`);
    failures += 1;
  }
}

/** Find the endpoint: `CGW_MCP_URL` -> runtime.json's mcpUrl -> command-line argument. */
function resolveEndpoint() {
  const fromArgs = process.argv[2]?.trim();
  if (fromArgs) {
    return fromArgs.replace(/\/+$/, "").endsWith("/mcp")
      ? fromArgs.replace(/\/+$/, "")
      : `${fromArgs.replace(/\/+$/, "")}/mcp`;
  }
  const explicit = process.env.CGW_MCP_URL?.trim();
  if (explicit) {
    return explicit.replace(/\/+$/, "").endsWith("/mcp")
      ? explicit.replace(/\/+$/, "")
      : `${explicit.replace(/\/+$/, "")}/mcp`;
  }
  for (const dir of [".", "..", "../src-tauri", "src-tauri", resolve(here, "../..")]) {
    try {
      const raw = JSON.parse(readFileSync(resolve(dir, "runtime.json"), "utf8"));
      if (typeof raw.mcpUrl === "string" && raw.mcpUrl) {
        return `${raw.mcpUrl.replace(/\/+$/, "")}/mcp`;
      }
    } catch {
      // keep looking
    }
  }
  throw new Error("no MCP endpoint found: set CGW_MCP_URL, or pass the endpoint as the first argument");
}

/** One JSON-RPC call (Streamable HTTP: `POST /mcp`). */
async function rpc(endpoint, body, extraHeaders = {}) {
  const response = await fetch(endpoint, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      ...extraHeaders,
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  // the spec allows two response shapes: plain JSON, or an SSE stream (`data: {...}`). Both must parse.
  let parsed = null;
  if (text.trim().startsWith("{")) {
    parsed = JSON.parse(text);
  } else {
    const line = text
      .split("\n")
      .filter((row) => row.startsWith("data:"))
      .pop();
    parsed = line ? JSON.parse(line.slice(5).trim()) : null;
  }
  return { status: response.status, body: parsed, raw: text };
}

async function main() {
  const endpoint = resolveEndpoint();
  const base = endpoint.replace(/\/mcp$/, "");
  console.log(`MCP endpoint: ${endpoint}`);

  const health = await (await fetch(`${base}/healthz`)).json();
  check(health.ok === true, `/healthz returned ok (${JSON.stringify(health)})`);
  check(
    typeof health.daemonPort === "number" && health.daemonPort > 0,
    `/healthz reported the daemon's own port: ${health.daemonPort}`,
  );
  check(health.mcp === true, "health check says mcp: true");
  // the tool count is no longer hard-coded in two languages (every new tool meant eight edits,
  // and it was missed once). Here we only assert the two sources agree and key tools remain;
  // the exact number is watched by the Rust tests.
  check(health.tools >= 37, `health check reported a tool count (${health.tools})`);

  console.log("\n--- MCP handshake ---");
  const init = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "http-test", version: "1.0.0" },
    },
  });
  check(init.status === 200, `initialize returned 200 (actual ${init.status})`, init.raw.slice(0, 200));
  check(
    init.body?.result?.serverInfo?.name === "compilegateway",
    `handshake ok: ${JSON.stringify(init.body?.result?.serverInfo)}`,
  );
  check(
    init.body?.result?.protocolVersion === "2024-11-05",
    `protocol version follows the client's (${init.body?.result?.protocolVersion})`,
  );
  check(
    typeof init.body?.result?.instructions === "string",
    "initialize carried instructions (an agent knows how to use it right away)",
  );
  check(
    init.body?.result?.capabilities?.tools !== undefined &&
      init.body?.result?.capabilities?.resources !== undefined,
    "capabilities declare both tools and resources",
  );

  // a notification has no id and must have no response body
  const notified = await rpc(endpoint, { jsonrpc: "2.0", method: "notifications/initialized" });
  check(notified.status === 202, `notifications/initialized returned 202 (actual ${notified.status})`);
  check(notified.raw.trim() === "", "the notification really has no response body");

  console.log("\n--- tools and resources ---");
  const tools = await rpc(endpoint, { jsonrpc: "2.0", id: 2, method: "tools/list" });
  const toolList = tools.body?.result?.tools ?? [];
  check(
    toolList.length === health.tools,
    `tools/list and /healthz agree (${toolList.length} vs ${health.tools})`,
  );
  const mustHave = [
    "cgw_status",
    "cgw_set_config",
    "cgw_reload_manifests",
    "cgw_manifest_status",
    "cgw_export_manifest",
    "cgw_save_manifest",
    "cgw_run_batch",
  ];
  for (const name of mustHave) {
    check(toolList.some((tool) => tool.name === name), `${name} is in the tool list`);
  }
  check(
    toolList.every((tool) => tool.inputSchema?.type === "object" && tool.description),
    "every tool has a description and an object schema",
  );
  const saveTask = toolList.find((tool) => tool.name === "cgw_save_task");
  check(
    Array.isArray(saveTask?.inputSchema?.properties?.task?.oneOf),
    "cgw_save_task's oneOf (required fields per kind) came through too",
  );
  // this property comes from "MCP lives in the daemon": $refs in tool schemas are already
  // expanded in place, so a client validating them never reports an unresolved reference
  check(
    !JSON.stringify(saveTask?.inputSchema ?? {}).includes("$ref"),
    "no dangling $ref in the tool schema (the daemon's copy is already expanded)",
  );

  const resources = await rpc(endpoint, { jsonrpc: "2.0", id: 3, method: "resources/list" });
  const resourceList = resources.body?.result?.resources ?? [];
  check(resourceList.length === 8, `resources/list returned 8 (6 schemas + guide + cli, actual ${resourceList.length})`);

  const read = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 4,
    method: "resources/read",
    params: { uri: "cgw://schema/task" },
  });
  const schema = JSON.parse(read.body?.result?.contents?.[0]?.text ?? "{}");
  check(
    schema.$schema === "http://json-schema.org/draft-07/schema#",
    `resources/read can fetch a schema (${schema.$schema})`,
  );

  // ---- the two teaching layers: playbook resources + CLI reference + prompt templates ----
  console.log("\n--- teaching resources (guide / cli) and prompts ---");
  const guide = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 12,
    method: "resources/read",
    params: { uri: "cgw://guide" },
  });
  const guideText = guide.body?.result?.contents?.[0]?.text ?? "";
  check(
    guideText.includes("Getting a project running for the first time") &&
      guideText.includes("What to do when it fails"),
    `cgw://guide is a real playbook page (${guideText.length} chars)`,
  );
  check(
    guide.body?.result?.contents?.[0]?.mimeType === "text/markdown",
    "guide's mimeType is markdown",
  );

  const cli = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 13,
    method: "resources/read",
    params: { uri: "cgw://cli" },
  });
  let cliDoc = {};
  try {
    cliDoc = JSON.parse(cli.body?.result?.contents?.[0]?.text ?? "{}");
  } catch {
    // the assertion below will fail and print the content
  }
  check(
    Array.isArray(cliDoc.commands) && cliDoc.commands.length >= 60,
    `cgw://cli carries the full command surface (${cliDoc.commands?.length} commands)`,
  );
  check(
    cliDoc.exitCodes && Object.keys(cliDoc.exitCodes).length === 8,
    "cgw://cli carries the exit code contract",
  );
  check(
    typeof cliDoc.whenToUseCli === "string" && cliDoc.whenToUseCli.includes("shell"),
    "cgw://cli explains when to use the CLI instead of MCP",
  );
  // same table as the CLI: spot-check that one entry must be there
  check(
    (cliDoc.commands ?? []).some((item) => item.name === "manifests.refresh"),
    "cgw://cli has manifests.refresh (same source as `cgw help --json`)",
  );

  const prompts = await rpc(endpoint, { jsonrpc: "2.0", id: 14, method: "prompts/list" });
  const promptList = prompts.body?.result?.prompts ?? [];
  check(promptList.length >= 3, `prompts/list returned ${promptList.length} templates`);
  check(
    promptList.every((item) => item.name && item.description),
    "every prompt has a name and a description",
  );
  const got = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 15,
    method: "prompts/get",
    params: { name: "first-build", arguments: { clientId: "c-demo" } },
  });
  const gotText = got.body?.result?.messages?.[0]?.content?.text ?? "";
  check(
    gotText.includes("push") && gotText.includes("batch"),
    "prompts/get returns the first-build walkthrough body",
  );
  const missingArg = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 16,
    method: "prompts/get",
    params: { name: "diagnose-job", arguments: {} },
  });
  check(
    missingArg.body?.error?.code === -32602 &&
      (missingArg.body?.error?.message ?? "").includes("jobId"),
    "a missing required argument names which one",
    JSON.stringify(missingArg.body).slice(0, 200),
  );

  console.log("\n--- really call the daemon once ---");
  const status = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 5,
    method: "tools/call",
    params: { name: "cgw_status", arguments: {} },
  });
  const text = (status.body?.result?.content ?? []).map((part) => part.text ?? "").join("\n");
  check(text.includes("CompileGateway"), `cgw_status works: ${text.split("\n")[0]?.slice(0, 80)}`);
  // a tool call goes through the daemon's own API, so the log source should be mcp
  const listed = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 7,
    method: "tools/call",
    params: { name: "cgw_list_clients", arguments: {} },
  });
  check(
    listed.body?.result?.isError !== true,
    "cgw_list_clients works too (so the tool-to-API mapping is intact)",
  );

  console.log("\n--- edge cases ---");
  const missing = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 6,
    method: "tools/call",
    params: { name: "cgw_job", arguments: { jobId: "j-nope" } },
  });
  check(
    missing.body?.result?.isError === true,
    "the error is wrapped as isError (nonexistent job)",
    JSON.stringify(missing.body).slice(0, 200),
  );
  check(
    missing.body?.result?.structuredContent?.error?.code === "jobNotFound",
    `the structured error came back too (${missing.body?.result?.structuredContent?.error?.code})`,
  );

  const unknown = await rpc(endpoint, { jsonrpc: "2.0", id: 8, method: "tools/call", params: { name: "cgw_nope" } });
  check(unknown.body?.result?.isError === true, "an unknown tool is wrapped as isError");

  const badMethod = await rpc(endpoint, { jsonrpc: "2.0", id: 9, method: "resources/subscribe" });
  check(badMethod.body?.error?.code === -32601, "an undeclared method returns -32601 (instead of pretending success)");

  const badResource = await rpc(endpoint, {
    jsonrpc: "2.0",
    id: 17,
    method: "resources/read",
    params: { uri: "cgw://nope" },
  });
  check(
    badResource.body?.error?.code === -32602 &&
      (badResource.body?.error?.message ?? "").includes("cgw://guide"),
    "an unknown resource lists the available ones (including the two prose resources)",
    JSON.stringify(badResource.body).slice(0, 200),
  );
  const badJson = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "{ not json",
  });
  check(badJson.status === 400, `bad JSON returns 400 (actual ${badJson.status})`);

  const getMcp = await fetch(endpoint, { method: "GET" });
  check(
    getMcp.status === 405,
    `GET /mcp returns 405 in stateless mode (actual ${getMcp.status})`,
  );
  const wrongPath = await fetch(`${base}/nope`, { method: "POST", body: "{}" });
  check(wrongPath.status === 404, `unknown path returns 404 (actual ${wrongPath.status})`);

  if (failures) {
    console.log(`\n${failures} checks did not pass`);
    process.exit(1);
  }
  console.log("\nall passed");
}

main().catch((error) => {
  console.error(`the test itself crashed: ${error.message}`);
  process.exit(1);
});
