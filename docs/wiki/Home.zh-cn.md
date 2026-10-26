# CompileGateway

CompileGateway 把你本机的源码目录**推**到一台 SSH 目标机(通常是一台 macOS 构建机),
在那边**构建**,再把产物**拉**回来 —— 或者直接打包成 zip 供导出。

一份核心实现,四个前端:浏览器界面、命令行客户端、给 AI agent 用的内置 MCP,以及它们共用的 HTTP 接口。

## 它做三件事

- **推送** —— 把本地项目目录送到目标机,带默认排除项,并对脚本做 CRLF→LF 归一化。
- **构建** —— 用目标机自己的工具链执行,构建方式由声明式的**构建类型**(manifest)描述:
  变量、有序步骤、产物路径。
- **把结果带回来** —— 产物拉回宿主、打包成 zip 导出,或者在任意一侧复制 / 剪切 / 删除。

## 四个前端,一份实现

| 前端 | 是什么 | 什么时候用 |
|---|---|---|
| **WebUI** | daemon 提供的浏览器界面 | 想看项目、任务状态、日志、改配置 |
| **CLI(`cgw`)** | HTTP 瘦客户端,从不交互 | 脚本、终端里、任何要重复做的事 |
| **MCP** | 内置在 daemon 里,跑在另一个端口 | 想让 AI agent 来驱动构建 |
| **HTTP 接口** | 上面三个都在用的那个接口 | 你要自己写客户端 |

**对等原则：WebUI 能做的，CLI 一定能做**；反向则根据使用场景做了适度收敛（如底层的 JSON Schema 查询、自动化流水线模板的命令行实例化等仅对 CLI、MCP 和 HTTP 开放，以保持图形界面的极简与专注）。

## 需要什么

构建需要 Rust(nightly)与 Node 18+;Windows 上的桌面外壳还要 WebView2;一台 SSH 目标机;
以及目标机上那套工具链(Xcode、Gradle……)。

## 快速开始

```bash
# 1. 先构建前端资源(它会被编进二进制)
cd webui-src && npm ci && npm run build && cd ..

# 2. 构建二进制
cd src-tauri && cargo build --release && cd ..

# 3. 起 headless daemon(或者起带窗口的外壳)
src-tauri/target/release/compilegateway-daemon

# 4. 让 CLI 找到它(大多数情况下它自己就找到了)
cgw doctor
```

然后是:加一台客户机(SSH 目标)→ 登记一个项目 → 推送 → 选或写一个构建类型 → 构建 →
拉回或导出产物。完整走一遍见[快速上手](Getting-Started.zh-cn)。

## 文档

| 页面 | 讲什么 |
|---|---|
| [快速上手](Getting-Started.zh-cn) | 构建、运行、第一个项目、第一次构建 |
| [概念与模型](Concepts.zh-cn) | 对象模型、job 生命周期、并发规则 |
| [任务](Tasks.zh-cn) | 七种任务、输入/输出契约、产物 |
| [批量与工作流](Batches-and-Workflows.zh-cn) | 有序的多步执行与跨项目模板 |
| [构建类型(manifest)](Manifests.zh-cn) | 一种工程"怎么构建"的声明 |
| [客户机与环境](Clients-and-Environment.zh-cn) | SSH 目标机、主机指纹、基准目录、环境探测 |
| [WebUI](WebUI.zh-cn) | 浏览器界面,一页一页讲 |
| [命令行](CLI.zh-cn) | `cgw`:命令、退出码、脚本化 |
| [MCP](MCP.zh-cn) | 内置的 AI agent 服务 |
| [HTTP 接口](HTTP-API.zh-cn) | 端点、鉴权、错误码 |
| [配置与日志](Configuration-and-Logs.zh-cn) | `config.json`、三种改法、日志文件 |
| [故障排查](Troubleshooting.zh-cn) | 症状、错误码、怎么试 |

## 速览

| | |
|---|---|
| 任务类型 | `build`、`push`、`pull`、`copy`、`move`、`delete`、`export` |
| 并发 | 一个项目同时一个 job;每项目 FIFO 队列 + `enqueue`;可选的主机级互斥 |
| MCP | 42 个工具、8 份资源、3 个 prompt,独立端口(默认 3994) |
| Schema | `manifest`、`task`、`batch`、`client`、`project`、`workflow` —— 由 Rust 结构体生成 |
| 落盘状态 | `config.json`、`runtime.json`、`projects.json`、`jobs.json`、`manifest/`、`workflows/`、`logs/` |
