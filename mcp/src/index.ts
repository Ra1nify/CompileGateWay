#!/usr/bin/env node
/**
 * CompileGateway MCP - a forwarding bridge from **stdio to Streamable HTTP**.
 *
 * The MCP adapter itself already lives inside the daemon (Rust, `src-tauri/src/mcp.rs`);
 * it listens on the daemon's second port at the fixed endpoint `POST /mcp`. This file is
 * not a second implementation: it only turns JSON-RPC lines on stdin into POSTs to the
 * daemon and writes the responses back to stdout.
 *
 * It still exists because MCP clients come in two kinds. Clients that can connect to an
 * HTTP address directly (`type: "http"`) do not need this file at all; clients that only
 * spawn a subprocess over stdio use this bridge.
 *
 * The payoff: the tool table exists in exactly one place. Every tool name, description
 * and argument schema comes from Rust via the daemon.
 *
 * Usage:
 *   node dist/index.js                                              # stdio, started by the client
 *   CGW_MCP_URL=http://127.0.0.1:3994/mcp node dist/index.js
 *
 * Endpoint resolution order (same idea as the CLI finding the daemon):
 *   1. `CGW_MCP_URL` (with or without /mcp, or just http://host:port)
 *   2. `mcpUrl` in `runtime.json` under `CGW_HOME` (default `~/.compilegateway`) --
 *      the daemon always writes a pointer file there, so it works from any cwd and
 *      handles the "MCP port is auto-chosen" case
 *   3. `mcpUrl` in `runtime.json` in the current dir / parent / src-tauri
 *   4. `mcp.port` in `config.json` in those same dirs
 *
 * For daemons that need credentials (MCP on wan with username/password) set
 * `CGW_USERNAME` / `CGW_PASSWORD`; the bridge then sends HTTP Basic.
 */

import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import process from "node:process";

/// `CGW_HOME` (default `~/.compilegateway`) - same rules as Rust `paths::cgw_home()`:
/// `CGW_HOME` wins; otherwise `USERPROFILE` (Windows) / `HOME` (Unix) plus `.compilegateway`.
function cgwHome(): string | null {
  const explicit = process.env.CGW_HOME?.trim();
  if (explicit) {
    return explicit;
  }
  const home = process.env.USERPROFILE?.trim() || process.env.HOME?.trim();
  if (!home) {
    return null;
  }
  return join(home, ".compilegateway");
}

/// Directories to try, in order. The shared dir comes first: the daemon writes a pointer
/// file there, so the bridge finds the daemon from any cwd the MCP client picks (same order as `cgw`'s `exe_dirs()`).
function searchDirs(): string[] {
  const dirs: string[] = [];
  const home = cgwHome();
  if (home) {
    dirs.push(home);
  }
  for (const relative of [".", "..", "../src-tauri", "src-tauri"]) {
    dirs.push(resolve(relative));
  }
  return [...new Set(dirs)];
}

/** The three sources tried in order; see the file header. */
function resolveEndpoint(): string {
  const explicit = process.env.CGW_MCP_URL?.trim();
  if (explicit) {
    return explicit.replace(/\/+$/, "").endsWith("/mcp")
      ? explicit.replace(/\/+$/, "")
      : `${explicit.replace(/\/+$/, "")}/mcp`;
  }

  const dirs = searchDirs();

  for (const dir of dirs) {
    try {
      const raw = JSON.parse(readFileSync(join(dir, "runtime.json"), "utf8"));
      if (typeof raw.mcpUrl === "string" && raw.mcpUrl) {
        return `${raw.mcpUrl.replace(/\/+$/, "")}/mcp`;
      }
    } catch {
      // not there, keep looking
    }
  }

  for (const dir of dirs) {
    try {
      const raw = JSON.parse(readFileSync(join(dir, "config.json"), "utf8"));
      const port = raw?.mcp?.port;
      if (typeof port === "number" && port > 0) {
        return `http://127.0.0.1:${port}/mcp`;
      }
    } catch {
      // same as above
    }
  }

  throw new Error(
    "Cannot find the MCP endpoint. Four options:\n" +
      "  * set CGW_MCP_URL (e.g. http://127.0.0.1:3994/mcp);\n" +
      "  * run this process next to the daemon (it reads runtime.json);\n" +
      "  * make sure CGW_HOME (default ~/.compilegateway) is readable -- the daemon writes a runtime.json there;\n" +
      "  * check the address in the WebUI config page / `cgw mcp url`, and confirm MCP is enabled.",
  );
}

function authHeader(): Record<string, string> {
  const username = process.env.CGW_USERNAME;
  const password = process.env.CGW_PASSWORD;
  if (!username || !password) {
    return {};
  }
  const token = Buffer.from(`${username}:${password}`, "utf8").toString("base64");
  return { authorization: `Basic ${token}` };
}

/** One JSON-RPC message (shape is not fixed, forwarded as-is). */
interface RpcMessage {
  jsonrpc?: string;
  id?: string | number | null;
  method?: string;
  params?: unknown;
  [key: string]: unknown;
}

/** The error object's message; `catch` gives us `unknown`. */
function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function post(endpoint: string, message: RpcMessage): Promise<RpcMessage | null> {
  const isNotification = message.id === undefined;
  const response = await fetch(endpoint, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      // the spec wants clients to accept both; the daemon is stateless and only returns JSON
      accept: "application/json, text/event-stream",
      ...authHeader(),
    },
    body: JSON.stringify(message),
  });

  // a notification's response body is empty (202), nothing to forward
  if (isNotification || response.status === 202) {
    return null;
  }

  const text = await response.text();
  if (!text.trim()) {
    return {
      jsonrpc: "2.0",
      id: message.id ?? null,
      error: { code: -32603, message: `daemon returned an empty response (HTTP ${response.status})` },
    };
  }
  try {
    return JSON.parse(text) as RpcMessage;
  } catch (error) {
    return {
      jsonrpc: "2.0",
      id: message.id ?? null,
      error: { code: -32603, message: `daemon did not return JSON: ${reason(error)}` },
    };
  }
}

function write(message: RpcMessage): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

async function main(): Promise<void> {
  const endpoint = resolveEndpoint();
  process.stderr.write(`CompileGateway MCP bridge -> ${endpoint}\n`);

  let buffer = "";
  // one at a time, in order: JSON-RPC allows concurrency, but all requests go to the
  // same daemon, so preserving order beats the little extra parallelism.
  let queue: Promise<void> = Promise.resolve();

  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk: string) => {
    buffer += chunk;
    let at = buffer.indexOf("\n");
    while (at >= 0) {
      const line = buffer.slice(0, at).trim();
      buffer = buffer.slice(at + 1);
      at = buffer.indexOf("\n");
      if (!line) {
        continue;
      }
      queue = queue.then(async () => {
        let message: RpcMessage;
        try {
          message = JSON.parse(line) as RpcMessage;
        } catch (error) {
          // not even JSON: answer with a parse error per the spec, do not swallow it
          write({
            jsonrpc: "2.0",
            id: null,
            error: { code: -32700, message: `request body is not valid JSON: ${reason(error)}` },
          });
          return;
        }
        try {
          const response = await post(endpoint, message);
          if (response) {
            write(response);
          }
        } catch (error) {
          if (message.id === undefined) {
            process.stderr.write(`failed to forward notification: ${reason(error)}\n`);
            return;
          }
          write({
            jsonrpc: "2.0",
            id: message.id ?? null,
            error: {
              code: -32603,
              message: `cannot reach the daemon's MCP endpoint (${endpoint}): ${reason(error)}`,
            },
          });
        }
      });
    }
  });

  process.stdin.on("end", () => {
    void queue.then(() => process.exit(0));
  });
}

main().catch((error: unknown) => {
  process.stderr.write(`${reason(error)}\n`);
  process.exit(1);
});
