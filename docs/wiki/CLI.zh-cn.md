# 命令行客户端 (`cgw`)

`cgw` 是 CompileGateway 的命令行瘦客户端，它自身不包含任何编译和业务逻辑，所有指令均翻译为对 Daemon 的 HTTP 请求。

## 客户端概览

每次调用 `cgw` 都会发起一次 HTTP 请求，并将返回的 JSON 渲染为适合终端查看的文本。客户端内部不进行任何编译、打包或直接 SSH 连接；若 Daemon 未启动，`cgw` 会输出错误提示并以退出码 2 退出。

- **二进制文件**：`compilegateway-cli`（Windows 下为 `compilegateway-cli.exe`），编译产物位于 `src-tauri/target/release/compilegateway-cli[.exe]`。文档与帮助信息中统一简称为 `cgw`。
- **构建方式**：`cargo build --release --bin compilegateway-cli`；Headless 无图形库构建：`cargo build --release --no-default-features --bin compilegateway-daemon --bin compilegateway-cli`。

使用 `cgw setup` 或 `cgw setup --apply` 检查并配置环境变量（Windows 下会自动将当前目录添加到用户 PATH，无需管理员权限；Linux/macOS 下仅打印推荐的 `export PATH=...` 指令）。

裸敲 `cgw` 会直接进入交互式 Shell 环境 —— 即桌面窗口的终端形态：上方实时滚动日志，下方提供 `cgw> ` 命令提示符。在非终端环境（管道、CI 任务）下，裸敲会自动打印命令帮助树而不会挂起。`cgw -shell`（或 `--shell` / `shell`）具有相同语义：复用已运行的 Daemon，未找到时自动拉起同级 `compilegateway-daemon`，并额外提供 Shell 专属的 `help`、`clear`、`logs`、`webui` 和 `exit` 指令。

## 服务地址解析链

每次运行 `cgw` 时按以下优先级解析 Daemon 地址，命中即止：

| 优先级 | 来源 | 说明 |
|---|---|---|
| 1 | `--url http://host:port` | 命令行单次显式指定，优先级最高 |
| 2 | `$CGW_URL` | 环境变量 |
| 3 | `runtime.json` in `CGW_HOME` | 用户级共享目录（默认 `~/.compilegateway`），Daemon 启动时自动写入 |
| 4 | `runtime.json` 同可执行文件目录 | 标准打包解压目录 |
| 5 | `runtime.json` 当前工作目录 | 方便在源码工作区内直接运行 |
| 6 | `config.json` 中的 `port` | 兜底方案（仅适用于固定端口，`port: 0` 自动挑选端口时不适用） |

`runtime.json` 具有最高权威性，因为它记录了 Daemon **实际**绑定的运行端口。

```bash
cgw doctor                                # 快速诊断：Daemon 状态、登录需求、统计数据
cgw setup                                 # 查看当前地址解析来源及 PATH 配置建议
cgw --url http://127.0.0.1:65251 status   # 显式覆盖地址
```

`cgw doctor` 独立执行各项健康检查，按行输出 `OK` / `FAIL` 与告警信息，全项通过退出码为 0，任一项异常退出码为 3。

## 全局通用选项

| 选项 | 说明 |
|---|---|
| `--url <url>` | 指定 Daemon 基础 URL，覆盖自动发现 |
| `--json` | 结构化输出；发生错误时在 stdout 统一输出错误 JSON |
| `--wait` | 异步任务跟踪至结束，以任务最终执行结果作为进程退出码 |
| `--timeout <seconds>` | `--wait` 超时时间（秒，默认 3600） |
| `--help`, `-h`, `help`, `?` | 树形或逐层帮助（如 `cgw help tasks save`、`cgw tasks save -h`） |
| `--version`, `-V` | 输出版本信息 |

## 鉴权机制

- **本地回环（Loopback）免认证**：来自 `127.0.0.1` / `::1` 的请求天然放行，即使设置了密码也无需登录。
- **远程访问一次性传参**：连接已启用鉴权的 WAN Daemon 时，可通过环境变量传入凭据，在首次收到 `401` 时自动完成认证，不落盘存储：
  ```bash
  CGW_USERNAME=alice CGW_PASSWORD=secret cgw --url http://build-box:65251 status
  ```
- **会话登录 (`cgw login` / `cgw logout`)**：换取会话 Cookie 并按主机保存在 `CGW_HOME/cgw-session.json` 中：
  ```bash
  cgw login --username alice --password secret
  cgw logout
  ```
- **绝对非交互式**：缺少凭据时直接以退出码 7 报错，绝不挂起等待用户控制台输入。

## 稳定退出码契约

脚本与自动化应基于退出码做分支判断，不要去 grep 自然语言输出：

| 退出码 | 名称 | 含义 |
|---|---|---|
| 0 | 成功 | 命令执行完成；对于 `--wait`，表示跟踪的 Job 执行成功 |
| 1 | 用法错误 | 未知命令、缺少参数、JSON 格式非法或数值校验失败 |
| 2 | 通信失败 | 无法连接到 Daemon（连接被拒、超时、未发现有效服务地址） |
| 3 | API 错误 | Daemon 返回业务错误（4xx/5xx）或 `doctor` 诊断未通过 |
| 4 | 项目繁忙 | HTTP 409 `projectBusy`：该项目已有其他 Job 正在执行 |
| 5 | Job 失败 | `--wait` 跟踪的任务最终状态为 `failed` / `cancelled`，或超时 |
| 6 | 资源不存在 | HTTP 404：项目 / 任务 / Batch / Job / Manifest ID 不存在 |
| 7 | 需要认证 | HTTP 401，或非本机尝试执行本机专属接口（`authLocalOnly`）时的 403 |

其他 403 错误（如路径越界 `outsideRoot`）归属于业务错误，退出码为 3。

## 脚本与 AI Agent 交互规范

- **绝对无交互阻断**：破坏性操作通过显式命令行参数确认（`--force`, `--overwrite`, `--clean`）。
- **`--json` 统一错误输出**：失败时 stdout 保证输出单行标准错误对象：
  ```json
  {"ok":false,"exitCode":4,"error":{"code":"projectBusy","httpStatus":409,
   "message":"项目忙","retryAfterSeconds":5,"jobId":"j-1a2b","projectId":"p-app"}}
  ```
- **多样化 JSON 管道输入**：`--body '<json>'`、`--file <path>` 或 `--file -`（标准输入），彻底避免 Shell 引号嵌套转义问题：
  ```bash
  cgw tasks save app --file - < task.json
  cat task.json | cgw tasks save app --file -
  ```
- **`--wait` 轮询跟踪**：未加 `--wait` 时立即返回 `jobId`；加了 `--wait` 后以 ~800ms 间隔轮询 `GET /api/jobs/{id}`，进度输出至 **stderr**，最终结果输出至 **stdout**，方便下游管道精准捕获产物。

## 命令分组全景

当前版本（v0.1.0）在 `cgw help --json` 中注册了 **80 条命令**，其中 71 条与 WebUI 界面入口一一对应。

### 状态与认证 (status, doctor, setup, login)

`status` · `doctor` · `setup [--apply]` · `login [--username U] [--password P]` · `logout`

```bash
cgw doctor --json
cgw setup
```

### 客户机与环境 (clients, basedir, env)

`clients [list]` · `clients add --host <ip> [--user U --password P | --key <path>] [--name N] [--port P] [--tags a,b] [--base-dir <path>]` · `clients edit <id> [...]` · `clients test <id>` · `clients hostkey <id> [--trust <key> | --forget | --import <file|-> [--force] | --export]` · `clients rm <id>` · `basedir <id> <path> [--no-verify]` · `env <id> [--cached]` · `env --script`

```bash
cgw clients add --name mac --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519
cgw clients test mac && cgw env mac
ssh-keyscan -p 2222 build-box | cgw clients hostkey c-mac --import -
```

### 目录浏览与项目文件 (browse, projects, tree, cat)

`browse [<path>]` · `projects [list]` · `projects add --client <id> --path <path> [--name N]` · `projects rm <id>` · `tree <project> [path] [--side remote|host]` · `cat <project> <path> [--side remote|host]`

```bash
cgw browse ~/src
cgw projects add --client c-mac --path ~/src/app --name app
cgw tree app Sources --side remote
cgw cat app README.md
```

### 任务管理与执行 (tasks, run)

`tasks [list] <project>` · `tasks save <project> --body '<json>' | --file <path> | -` · `tasks rm <project> <taskId>` · `tasks preview <project> <taskId>` · `run <project> <taskId> [--wait] [--timeout N] [--enqueue]`

```bash
cgw tasks app
cgw tasks preview app build-app      # 仅预览渲染后的命令与路径，不实际运行
cgw run app build-app --wait
```

### 批量工作流与跨项目模板 (batches, workflows)

`batches <project>` · `batch save <project> <name> <taskId...> | --body '<json>'` · `batch run <project> <batchId|name> [--from N] [--wait] [--enqueue]` · `batch rm <project> <batchId>` · `workflows [list]` · `workflows show <id>` · `workflows save <id> --body '<json>' | --file <path> | -` · `workflows rm <id>` · `workflows apply <project> <id> [--set k=v ...] [--apply] [--overwrite] [--run] [--name N]` · `workflows refresh`

```bash
cgw batch save app release push-src build-app pull-out
cgw batch run app release --wait
cgw workflows apply app push-build-pull --set scheme=Runner --apply --run --wait
```

### 同步与导出 (push, pull, export, exports)

`push <project> [--clean] [--keep-crlf] [--wait]` · `pull <project> [--wait]` · `export <project> [--source remote|host] [--path P] [--name N]` · `exports <project>` · `exports <project> <name> --save <path>`

```bash
cgw push app --wait
cgw export app --source remote --path build/output --name app-release
cgw exports app app-release.zip --save ./app-release.zip
```

### Job 历史与审计 (jobs)

`jobs [list]` · `job <jobId>` · `jobs logs <jobId> [--since N]` · `jobs cancel <jobId>` · `jobs retry <jobId> [--from N|--from-start] [--wait]`

```bash
cgw jobs
cgw job j-1a2b
cgw jobs logs j-1a2b --since 0
```

### 构建类型与 Schema (manifests, schema)

`manifests [list]` · `manifests show <id>` · `manifests save <id> --file <path> | -` · `manifests rm <id>` · `manifests status` · `manifests templates` · `manifests reset [id...]` · `manifests import <file.json> [--id <name>] [--force]` · `manifests export <id> [--save <path>]` · `manifests detect <dir>` · `manifests refresh` · `manifests schema` · `schema <manifest|task|batch|client|project|workflow>`

```bash
cgw manifests detect ~/src/app
cgw manifests refresh
cgw schema task
```

### 日志与系统配置 (logs, config, set)

`logs [days]` · `logs <YYYY-MM-DD> [--source S] [--scope X] [--limit N]` · `logs client <id> [--since N]` · `config [show]` · `config set [...]` · `set port|listen|user|language|login-interval|log-retain|roots|mcp|host-lock|all`

```bash
cgw logs client c-mac --since 0
cgw logs 2026-01-31 --source cli --limit 200
cgw set roots add /srv/builds
cgw set host-lock on
```

### MCP 与底层 API 兜底 (mcp, api)

`mcp [status]` · `mcp url` · `mcp tools [<name>]` · `mcp enable|disable` · `mcp port <N>` · `mcp listen lan|wan` · `api <METHOD> <PATH> [JSON]`

```bash
cgw mcp url
cgw mcp tools cgw_run_task
cgw api POST /api/projects/p-app/tasks/t-build/run '{"enqueue":true}'
```

详见 [MCP](MCP.zh-cn) 与 [HTTP 接口](HTTP-API.zh-cn)。

## 自描述帮助系统

帮助信息不是手写的静态文本，而是由核心数据结构动态生成的四种视图：

```bash
cgw help               # 顶层分组树
cgw help tasks         # 指定分组用法及 WebUI 映射
cgw help tasks save    # 具体叶子命令参数与说明
cgw help --json        # 机器可读格式（命令、参数、退出码契约）
cgw help --webui       # WebUI 界面入口 → cgw 命令对照表
```

## 典型操作场景

```bash
# 完整首次构建闭环：登记客户机 -> 连通性与环境验证 -> 登记项目 -> 推送源码 -> 构建 -> 拉回产物
cgw clients add --name mac --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519
cgw clients test mac && cgw env mac
cgw projects add --client c-mac --path ~/src/app --name app
cgw push app --wait
cgw run app build-app --wait
cgw pull app --wait

# 组合批量流水线一步执行
cgw batch save app release push-src build-app
cgw batch run app release --wait

# 打包导出 ZIP
cgw export app --source remote --path build/output --name app-release
cgw exports app app-release.zip --save ./app-release.zip

# 实时增量日志跟踪
cgw logs client c-mac --since 0

# 排队执行任务
cgw run app build-app --enqueue --wait
```

---

← [首页](Home.zh-cn)
