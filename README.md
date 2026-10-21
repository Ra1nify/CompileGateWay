# CompileGateway

<div align="center">

**Cross-Host Remote Build and Compilation Gateway for Developers and AI Agents**

*将本地源码无缝推送到远程 SSH 目标机（如 macOS / Linux 构建机）完成编译并安全取回产物的现代化网关*

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75+-orange.svg)](https://www.rust-lang.org/)
[![Model Context Protocol](https://img.shields.io/badge/MCP-Compatible-green.svg)](https://modelcontextprotocol.io/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)]()

[English](#english) • [中文说明](#chinese) • [GitHub Wiki](https://github.com/Ra1nify/CompileGateWay/wiki)

</div>

---

<a name="english"></a>
## Overview

**CompileGateway** solves a common development friction: your primary coding environment (e.g. Windows or Linux PC) is separate from your build and packaging targets (e.g. a remote macOS mini running Xcode, or a Linux CI workstation).

Instead of requiring fragile network shares, heavy CI/CD infrastructure, or custom agents running on the target machine, CompileGateway acts as a lightweight, secure orchestrator over standard **SSH/SFTP**. It enables humans (via WebUI and CLI) and **AI Agents** (via Model Context Protocol) to seamlessly:

1. **Push** local source code to a remote target with automatic Unix script normalization (CRLF -> LF).
2. **Execute** controlled multi-step build tasks (Xcode, Gradle, Cargo, Flutter, etc.).
3. **Pull** built binaries, archives, and artifacts back to the host machine.
4. **Inspect** remote directories, toolchain environments, and step execution logs in real time.

```
       +--------------------------------------------------------------+
       |                        Control Layer                         |
       |   +----------------+   +----------------+   +------------+   |
       |   | React WebUI    |   | CLI (`cgw`)    |   |  AI Agent  |   |
       |   | (Fluent UI)    |   | (Terminal App) |   |  (via MCP) |   |
       |   +-------+--------+   +-------+--------+   +-----+------+   |
       +-----------|--------------------|------------------|----------+
                   |                    |                  |
                   +--------------------+------------------+
                                        | (HTTP / SSE)
       +--------------------------------v-----------------------------+
       |               CompileGateway Daemon (Headless)               |
       |   - State & Projects Registry (`projects.json`)              |
       |   - Build Manifest Engine (`manifests/`)                     |
       |   - FIFO Concurrency Queue & Host-Level Mutex Lock           |
       |   - Built-in MCP Server (`http://127.0.0.1:3994/sse`)        |
       +--------------------------------+-----------------------------+
                                        | (Pure Rust SSH / SFTP)
                                        | [TOFU Host Key Verification]
       +--------------------------------v-----------------------------+
       |                     Remote Target Machine                    |
       |           (macOS Mini / Linux Server / Remote Host)          |
       |   - Standard OpenSSH Server (Zero remote agent required!)    |
       |   - Sandboxed Base Directory (`~/CompileGateway/<slug>/`)     |
       +--------------------------------------------------------------+
```

---

## Key Features

- **Strict Triple-Surface Parity**:
  $$\text{WebUI} \subseteq \text{CLI} == \text{HTTP API} == \text{MCP Tool}$$
  Every action available in the GUI can be driven from the command line (`cgw`), an HTTP request, or an LLM Agent.
- **First-Class AI Agent Integration**:
  Built-in MCP (Model Context Protocol) server. Compatible with **Claude Desktop**, **Cursor**, **Antigravity**, and any MCP client. Agents can autonomously query toolchains, register tasks, trigger builds, and stream logs.
- **Zero-Dependency Remote Target**:
  Nothing to install on the target machine! Works with any standard POSIX / macOS host with OpenSSH enabled.
- **Trust On First Use (TOFU) Security**:
  Validates host public key fingerprints on every SSH session, preventing MITM attacks with fine-grained approval dialogues.
- **Fail-Fast Pipelines & Resumable Retries**:
  Batch workflows stop immediately on error and can be resumed from the failed step (`cgw jobs retry <jobId>`) without repeating finished steps.
- **Project & Host Concurrency Locking**:
  Prevents competing builds from clobbering identical workspaces, supporting automatic queueing (`--enqueue`).
- **Shebang & CRLF Normalization**:
  Automatically converts Windows line endings (`CRLF`) to Unix (`LF`) for shell and script files on push, preventing `/bin/sh^M: bad interpreter` headaches.

---

## Quick Start

### 1. Build and Run

CompileGateway is written in Rust with a React (Fluent UI) frontend embedded into the binary.

```bash
# Clone the repository
git clone https://github.com/Ra1nify/CompileGateWay.git
cd CompileGateWay

# Build WebUI frontend
cd webui-src
npm install
npm run build
cd ..

# Build & Run Daemon
cargo run --manifest-path src-tauri/Cargo.toml --bin compilegateway-daemon
```

*For GUI window on desktop: `cargo run --manifest-path src-tauri/Cargo.toml --bin compilegateway`*

### 2. Configure CLI (`cgw`)

The `cgw` CLI interacts with the daemon. Add the binary directory to your `PATH`, or run:

```bash
cgw doctor
```

```text
CompileGateway 0.1.0 — HTTP thin client for daemon
Daemon: http://127.0.0.1:3993 (connected)
Auth: Not required (local mode)
Clients: 1 configured
Projects: 1 registered
Manifests: 6 available
```

### 3. Add a Client & Run a Build

```bash
# 1. Add SSH client machine (password or key)
cgw clients add --host 192.168.1.100 --user builder --key ~/.ssh/id_ed25519 --name mac-mini

# 2. Test SSH connection & record TOFU host fingerprint
cgw clients test mac-mini

# 3. Register a local project directory
cgw projects add --client mac-mini --path "D:/Projects/MyApp" --name MyApp

# 4. Push source, run build, and pull artifacts
cgw push MyApp --wait
cgw run MyApp build --wait
cgw pull MyApp --wait
```

---

## AI Agent Integration (MCP)

CompileGateway includes a native Model Context Protocol (MCP) server running on port `3994` (configurable).

### Claude Desktop Configuration

Add the following to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "compilegateway": {
      "command": "node",
      "args": [
        "/path/to/CompileGateWay/mcp/dist/index.js",
        "--url",
        "http://127.0.0.1:3994"
      ]
    }
  }
}
```

The AI Agent will automatically have access to tools such as:
- `cgw_env`: Inspect remote build tools (Xcode, JDK, Flutter, Cargo).
- `cgw_run_task`: Execute build, push, pull, or archive tasks.
- `cgw_run_batch`: Execute complete pipelines.
- `cgw_tree` & `cgw_cat`: Read files and directory listings safely.

---

<a name="chinese"></a>
## 中文说明

**CompileGateway** 专为解决跨平台跨主机编译痛点而设计。当你的主力工作站（如 Windows / Linux）需要将工程推送到远程机器（如 Mac mini 编译机）完成构建与产物收集时，无需搭建复杂的 CI 节点，CompileGateway 即可通过纯 SSH/SFTP 管道提供全自动化的调度网关。

### 核心亮点

1. **三态合一契约**：WebUI 界面、CLI 命令行（`cgw`）、HTTP API 与 AI Agent（MCP）拥有 100% 对齐的指令集。
2. **AI Agent 原生支持**：内置 MCP 服务器，无缝接入 Claude Desktop、Cursor、Antigravity，让 AI 自主执行跨主机编译排错。
3. **远端零依赖**：远程机器仅需开启标准 SSH 服务，不需要安装任何 Agent 或守护进程。
4. **企业级安全（TOFU）**：严格校验主机公钥指纹，凭据只写不读，绝不回传明文密码。
5. **智能换行转换**：推送代码时自动将携带 Shebang 的脚本换行符转换为 LF，杜绝 Windows 导致的 `\r` 执行异常。

### 常用命令速查

```bash
cgw doctor                  # 诊断环境与 Daemon 联通性
cgw status                  # 查看运行状态
cgw clients [list]          # 列出所有客户机
cgw clients test <id>       # 测试 SSH 连接并记录公钥指纹
cgw projects add            # 登记本地项目
cgw push <project> --wait   # 推送源码
cgw run <project> <task>    # 执行指定任务 (--wait 等待完成)
cgw pull <project> --wait   # 拉回产物
cgw mcp url                 # 查看 AI Agent MCP 端点地址
cgw help --webui            # 查看 CLI 命令与 WebUI 界面的位置映射表
```

---

## Documentation

Full documentation, concepts, and troubleshooting manuals are available in the **[CompileGateway GitHub Wiki](https://github.com/Ra1nify/CompileGateWay/wiki)**:

- 📖 [Home / 首页](https://github.com/Ra1nify/CompileGateWay/wiki/Home)
- 🚀 [Getting Started / 快速上手](https://github.com/Ra1nify/CompileGateWay/wiki/Getting-Started)
- 💡 [Core Concepts / 核心概念](https://github.com/Ra1nify/CompileGateWay/wiki/Concepts)
- 🖥️ [CLI Reference / 命令行手册](https://github.com/Ra1nify/CompileGateWay/wiki/CLI)
- 🌐 [WebUI Guide / 网页界面使用指南](https://github.com/Ra1nify/CompileGateWay/wiki/WebUI)
- 🤖 [MCP & Agent Protocol / AI 智能体集成](https://github.com/Ra1nify/CompileGateWay/wiki/MCP)
- 🛠️ [Troubleshooting / 常见故障排查](https://github.com/Ra1nify/CompileGateWay/wiki/Troubleshooting)

---

## License

CompileGateway is dual-licensed:
- **Open Source Edition**: GNU Affero General Public License v3.0 ([AGPL-3.0](LICENSE)).
- **Commercial Edition**: Available for commercial terms, closed-source integration, or exemption from copyleft obligations. See [LICENSE](LICENSE) for details.
