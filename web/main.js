// Frontend logic for the CLI-style window (no framework, no build step).
//
// Data sources:
//   invoke('get_logs')     log snapshot taken at startup
//   listen('cgw://log')    incremental log events after that
//   invoke('run_command')  commands submitted from the input box
//
// Both are merged and de-duplicated by seq, so nothing repeats and nothing is lost.

const TAURI = window.__TAURI__;
const invoke = TAURI?.core?.invoke;
const listen = TAURI?.event?.listen;

// Strings for this window itself (log text lives on the Rust side, src/i18n.rs)
const CHROME = {
  "zh-Hans": {
    placeholder: "输入命令,help 查看可用命令",
    hint: "Enter 执行 · ↑↓ 历史 · Ctrl+L 清屏 · 拖动可选中复制 · Ctrl+Shift+C 复制全部 · 点击右上角打开 WebUI",
    chipIdle: "WebUI 未启动",
    instancePlaceholder: "输入命令,help 查看可用命令(status / test / exit)",
    instanceHint: "这是该客户机的独立诊断窗口 · Enter 执行 · ↑↓ 历史 · 拖动可选中复制 · exit 关闭本窗口",
    copied: (n) => `${n} 行日志已复制到剪贴板`,
    copyFailed: "复制失败(剪贴板不可用)",
  },
  "en-US": {
    placeholder: "Type a command, `help` for the list",
    hint: "Enter to run · ↑↓ history · Ctrl+L clear · drag to select & copy · Ctrl+Shift+C copies all · click the chip for the WebUI",
    chipIdle: "WebUI not running",
    instancePlaceholder: "Type a command, `help` for the list (status / test / exit)",
    instanceHint: "Diagnostic window for this client · Enter to run · ↑↓ history · drag to select & copy · exit closes it",
    copied: (n) => `${n} log line${n === 1 ? "" : "s"} copied to clipboard`,
    copyFailed: "Failed to copy (clipboard unavailable)",
  },
};

const logEl = document.getElementById("log");
const cmdEl = document.getElementById("cmd");
const hintEl = document.getElementById("hint");
const chipEl = document.getElementById("chip");
const titleEl = document.getElementById("title");

let lastSeq = 0;
let ready = false;
const pending = [];
const history = [];
let histIdx = 0;

function fmtTime(ms) {
  if (!ms) return "";
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function atBottom() {
  return logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 40;
}

function append(entry) {
  if (!entry || entry.seq <= lastSeq) return;
  lastSeq = entry.seq;

  const stick = atBottom();
  const line = document.createElement("div");
  line.className = `line ${entry.level}`;

  const time = document.createElement("span");
  time.className = "time";
  time.textContent = fmtTime(entry.ts_ms);

  const text = document.createElement("span");
  text.className = "text";
  text.textContent = entry.text;

  line.append(time, text);
  logEl.append(line);
  if (stick) logEl.scrollTop = logEl.scrollHeight;
}

function applyLanguage(status) {
  const c = CHROME[status.lang] || CHROME["en-US"];
  const isInstance = Boolean(status.instance);
  document.documentElement.lang = status.lang;
  cmdEl.placeholder = isInstance ? c.instancePlaceholder : c.placeholder;
  hintEl.textContent = isInstance ? c.instanceHint : c.hint;
  titleEl.textContent = isInstance
    ? `${status.app} — ${status.instance.name}`
    : status.app;

  if (status.url) {
    chipEl.textContent = status.url.replace(/^https?:\/\//, "");
    chipEl.title = status.url;
    chipEl.classList.remove("fatal");
    chipEl.disabled = false;
  } else {
    chipEl.textContent = c.chipIdle;
    chipEl.title = "";
    chipEl.classList.add("fatal");
    chipEl.disabled = true;
  }
}

async function runCommand(value) {
  const outcome = await invoke("run_command", { input: value });
  if (outcome.action === "clear") {
    logEl.replaceChildren();
  } else if (outcome.action === "exit") {
    // Give the last log line a chance to render before quitting
    setTimeout(() => invoke("quit"), 90);
  }
}

function submit() {
  const value = cmdEl.value;
  cmdEl.value = "";
  if (!value.trim()) return;
  if (history[history.length - 1] !== value) history.push(value);
  histIdx = history.length;
  runCommand(value);
}

cmdEl.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    submit();
    return;
  }
  if (e.key === "ArrowUp") {
    e.preventDefault();
    if (histIdx > 0) histIdx -= 1;
    cmdEl.value = history[histIdx] ?? "";
    return;
  }
  if (e.key === "ArrowDown") {
    e.preventDefault();
    histIdx = Math.min(histIdx + 1, history.length);
    cmdEl.value = history[histIdx] ?? "";
    return;
  }
  if (e.key.toLowerCase() === "l" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    runCommand("clear");
  }
});

chipEl.addEventListener("click", () => {
  if (!chipEl.disabled) runCommand("webui");
});

// Clicking anywhere returns focus to the input box -- EXCEPT in the log pane.
//
// Any mousedown used to call `cmdEl.focus()`. In the log pane that broke text
// selection (it forms while dragging, so it is still empty at mousedown), and the
// pane exists precisely so a block can be selected and pasted elsewhere -- so its
// mouse events are left to the browser's native selection.
document.addEventListener("mousedown", (e) => {
  if (logEl.contains(e.target)) return;
  if (window.getSelection()?.toString()) return;
  if (e.target === chipEl) return;
  setTimeout(() => cmdEl.focus(), 0);
});

// Ctrl+C in the log pane goes to the system copy; all this does is avoid stealing
// focus (handled by mousedown above) and add a copy-everything shortcut.
document.addEventListener("keydown", (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (!mod || e.key.toLowerCase() !== "c") return;
  if (!e.shiftKey) return; // Ctrl+Shift+C = copy all logs
  const all = [...logEl.querySelectorAll(".line .text")]
    .map((node) => node.textContent ?? "")
    .join("\n");
  if (!all) return;
  e.preventDefault();
  const c = CHROME[document.documentElement.lang] || CHROME["en-US"];
  navigator.clipboard?.writeText(all).then(
    () => hintEl.textContent = c.copied(all.split("\n").length),
    () => hintEl.textContent = c.copyFailed,
  );
});

async function init() {
  // Show a visible notice when the Tauri injection is missing (e.g. no matching
// capability) instead of a blank window
  if (typeof invoke !== "function" || typeof listen !== "function") {
    throw new Error("Tauri IPC unavailable — check capabilities/default.json for this window");
  }

  const status = await invoke("get_status");
  applyLanguage(status);

  // Subscribe before taking the snapshot so no log line in between is lost
  await listen("cgw://log", (event) => {
    if (ready) append(event.payload);
    else pending.push(event.payload);
  });

  const snapshot = await invoke("get_logs");
  ready = true;
  [...snapshot, ...pending].sort((a, b) => a.seq - b.seq).forEach(append);

  logEl.scrollTop = logEl.scrollHeight;
  cmdEl.focus();
}

window.addEventListener("DOMContentLoaded", () => {
  init().catch((err) => {
    const line = document.createElement("div");
    line.className = "line error";
    const text = document.createElement("span");
    text.className = "text";
    text.textContent = `frontend init failed: ${err}`;
    line.append(text);
    logEl.append(line);
    hintEl.textContent = String(err);
  });
});
