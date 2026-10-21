#!/usr/bin/env node
/**
 * Concurrency stress test: verifies the design doc section 12 Phase 6 acceptance criterion -
 * "a project-level lock returns busy under high-frequency calls instead of producing a
 * corrupted build".
 *
 * Approach: fire N run_task calls concurrently at the same project, expecting
 *   * exactly one to get the lock (jobId),
 *   * all the others to be 409 projectBusy, never two jobs running at once.
 * Then switch to different projects concurrently and expect them not to affect each other.
 *
 * Usage: node test/hammer.mjs [concurrency]
 */

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const serverPath = resolve(here, "../dist/index.js");
const concurrency = Number.parseInt(process.argv[2] ?? "8", 10);

class StdioClient {
  constructor(child) {
    this.child = child;
    this.buffer = "";
    this.pending = new Map();
    this.nextId = 1;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => this.onData(chunk));
    child.on("exit", () => {
      for (const [, reject] of this.pending) reject(new Error("server exited"));
      this.pending.clear();
    });
  }
  onData(chunk) {
    this.buffer += chunk;
    let index;
    while ((index = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, index).trim();
      this.buffer = this.buffer.slice(index + 1);
      if (!line) continue;
      const message = JSON.parse(line);
      const waiter = this.pending.get(message.id);
      if (waiter) {
        this.pending.delete(message.id);
        message.error ? waiter.reject(new Error(JSON.stringify(message.error))) : waiter.resolve(message.result);
      }
    }
  }
  request(method, params) {
    const id = this.nextId++;
    return new Promise((done, reject) => {
      this.pending.set(id, { resolve: done, reject });
      this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }
  notify(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
  }
}

async function main() {
  const child = spawn(process.execPath, [serverPath], { stdio: ["pipe", "pipe", "pipe"] });
  const client = new StdioClient(child);
  await client.request("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "hammer", version: "1.0.0" },
  });
  client.notify("notifications/initialized", {});

  const call = async (name, args = {}) =>
    client.request("tools/call", { name, arguments: args });
  const text = (result) => result.content.map((part) => part.text ?? "").join("\n");
  const parse = (result) => JSON.parse(text(result));

  const home = process.env.USERPROFILE ?? process.env.HOME;
  const { mkdirSync, writeFileSync, rmSync } = await import("node:fs");

  const made = { clients: [], projects: [], dirs: [] };

  // create two projects, to test "same project excludes" and "across projects parallel" separately
  const projects = [];
  for (const tag of ["a", "b"]) {
    const dir = resolve(home, `cgw-hammer-${tag}`);
    mkdirSync(resolve(dir, "src"), { recursive: true });
    mkdirSync(resolve(home, "CompileGateway", `cgw-hammer-${tag}`, "src"), { recursive: true });
    writeFileSync(resolve(home, "CompileGateway", `cgw-hammer-${tag}`, "src", "x.txt"), "x");
    made.dirs.push(dir, resolve(home, "CompileGateway", `cgw-hammer-${tag}`));

    const clientInfo = parse(
      await call("cgw_save_client", {
        name: `hammer-${tag}`,
        host: "127.0.0.1",
        port: 22,
        auth: { method: "password", username: "x", password: "y" },
      }),
    );
    made.clients.push(clientInfo.client.id);

    const project = parse(
      await call("cgw_create_project", { clientId: clientInfo.client.id, sourcePath: dir }),
    );
    made.projects.push(project.project.id);

    await call("cgw_save_task", {
      projectId: project.project.id,
      task: {
        kind: "copy",
        name: `hammer-copy-${tag}`,
        from: { side: "host", path: "src" },
        to: { side: "host", path: `out-${Date.now()}` },
        overwrite: true,
      },
    });
    // cgw_list_tasks returns **all** projects, so pick out our own by id
    const all = parse(await call("cgw_list_tasks", { projectId: project.project.id }));
    const mine = all.projects.find((item) => item.id === project.project.id);
    const taskId = mine.tasks.at(-1).id;
    projects.push({ projectId: project.project.id, taskId });
  }

  // make the task slow (a few thousand small files) so the concurrency is real -
  // a task that finishes in milliseconds would have released the lock before later requests arrive.
  for (const tag of ["a", "b"]) {
    const src = resolve(home, "CompileGateway", `cgw-hammer-${tag}`, "src");
    for (let index = 0; index < 4000; index++) {
      writeFileSync(resolve(src, `f${String(index).padStart(5, "0")}.txt`), "x");
    }
  }

  // ---- 1. same project concurrently: two jobs must not run at once ----
  console.log(`same project, ${concurrency} concurrent run_task calls:`);
  const sameResults = await Promise.all(
    Array.from({ length: concurrency }, () => call("cgw_run_task", projects[0])),
  );
  const acceptedIds = sameResults.filter((r) => !r.isError).map((r) => parse(r).jobId);
  const busy = sameResults.filter((r) => r.isError && text(r).includes("projectBusy"));
  const other = sameResults.filter((r) => r.isError && !text(r).includes("projectBusy"));
  console.log(`  got the lock = ${acceptedIds.length}`);
  console.log(`  project busy = ${busy.length}`);
  if (other.length) {
    console.log(`  other errors = ${other.length}: ${text(other[0]).split("\n")[0]}`);
  }
  if (acceptedIds.length < 1 || acceptedIds.length + busy.length !== concurrency || other.length) {
    throw new Error("wrong concurrency outcome: only 'got the lock' and 'project busy' are allowed, and at least one must get it");
  }
  if (busy.length) {
    const busyText = text(busy[0]);
    console.log(
      `  info in the busy error body: ${busyText.split("\n").find((line) => line.includes("job")) ?? busyText.split("\n")[0]}`,
    );
  }

  // **This is what the doc really requires**: jobs of the same project must not overlap in time.
  // How many got the lock does not matter (when tasks are fast, later requests should succeed
  // anyway); what matters is that no two execution intervals intersect.
  const jobs = [];
  for (const jobId of acceptedIds) {
    for (let attempt = 0; attempt < 200; attempt++) {
      const job = parse(await call("cgw_job", { jobId }));
      if (job.state !== "running") {
        if (job.state !== "ok") throw new Error(`job failed: ${job.message}`);
        jobs.push(job);
        break;
      }
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  if (jobs.length !== acceptedIds.length) {
    throw new Error("some job never finished");
  }
  jobs.sort((a, b) => a.startedAt - b.startedAt);
  for (let index = 1; index < jobs.length; index++) {
    const previous = jobs[index - 1];
    const current = jobs[index];
    if (current.startedAt < previous.finishedAt) {
      throw new Error(
        `two jobs of the same project overlapped in time: ${previous.id} [${previous.startedAt},${previous.finishedAt}) ` +
          `vs ${current.id} [${current.startedAt},${current.finishedAt})`,
      );
    }
  }
  console.log(`  ${jobs.length} job execution intervals are pairwise non-overlapping ✓ (project-level lock works)`);
  const firstJob = jobs[0].id;
  void firstJob;

  // ---- 2. different projects concurrently: must not affect each other ----
  console.log(`\ndifferent projects concurrently (${projects.length} projects, 1 call each):`);
  const crossResults = await Promise.all(projects.map((project) => call("cgw_run_task", project)));
  const crossOk = crossResults.filter((result) => !result.isError);
  console.log(`  all got the lock = ${crossOk.length} / ${projects.length} (expected all)`);
  if (crossOk.length !== projects.length) {
    throw new Error(`cross-project concurrency was wrongly mutually exclusive: ${text(crossResults.find((r) => r.isError))}`);
  }
  // cross-project jobs **may** overlap - otherwise the lock granularity is wrong (a global lock)
  const crossJobs = [];
  for (const result of crossResults) {
    const jobId = parse(result).jobId;
    for (let attempt = 0; attempt < 200; attempt++) {
      const job = parse(await call("cgw_job", { jobId }));
      if (job.state !== "running") {
        if (job.state !== "ok") throw new Error(`cross-project job failed: ${job.message}`);
        crossJobs.push(job);
        break;
      }
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  const overlapped = crossJobs.some((job, index) =>
    crossJobs.some(
      (other, otherIndex) =>
        index !== otherIndex &&
        job.startedAt < other.finishedAt &&
        other.startedAt < job.finishedAt,
    ),
  );
  console.log(
    `  all ${crossJobs.length} jobs finished normally; in time ${overlapped ? "they overlapped (so it is not a global lock) ✓" : "no overlap was observed (tasks too fast, no assertion)"}`,
  );

  // ---- cleanup ----
  for (const projectId of made.projects) await call("cgw_delete_project", { id: projectId });
  for (const id of made.clients) await call("cgw_delete_client", { id });
  for (const dir of made.dirs) rmSync(dir, { recursive: true, force: true });
  console.log("\ncleanup done. project-level lock behaves correctly.");
  child.kill();
}

main().catch((error) => {
  console.error(`\nfailed: ${error.message}`);
  process.exit(1);
});
