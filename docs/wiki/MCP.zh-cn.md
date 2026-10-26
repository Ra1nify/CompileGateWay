# Model Context Protocol (MCP)

CompileGateway 在 Daemon 内部以 Rust 原生实现了 Model Context Protocol (MCP) 服务，并在独立的端口上对外暴露，使 AI Agent（Claude Code、Cursor、Cline、Codex 等）能够结构化地编排和执行远程跨机编译全链路。

## 架构概览

```
┌───────────────────────────────────────────────────────────┐
│                      CompileGateway                       │
│                                                           │
│  HTTP 主服务 (:65251)         内置 MCP 原生服务 (:3994)   │
│  ├─ WebUI 网页               ├─ 42 个专用工具             │
│  ├─ cgw 命令行客户端          ├─ 8 份核心资源              │
│  └─ REST 数据接口            └─ 3 个 Prompt 引导模板      │
└──────────────┬─────────────────────────────┬──────────────┘
               │                             │
       (本地环回 / 局域网)         Streamable HTTP / stdio 管道
               │                             │
        浏览器 / 终端控制台             AI 编程助手 / Agent 客户端
```

- **Daemon 原生内置（Rust）**：无须依赖外部 Node.js 运行时即可提供 MCP 服务；启动 Daemon 时 MCP 服务随同就绪。
- **独立服务端口**：运行在独立的端口（默认 `3994`，由 `mcp.port` 与 `mcp.listen` 配置），MCP 配置变更支持**即时热重载**，无须重启 Daemon。
- **传输协议**：标准 Streamable HTTP（`POST /mcp`，无状态 JSON-RPC 2.0）。
- **可选 stdio 转发桥（`mcp/`）**：附带轻量 Node.js stdio 转发脚本，仅用于适配只支持标准输入输出管道的客户端（如 Claude Desktop）；支持 HTTP 传输的客户端可直连端点。

## 配置 MCP 客户端

### Streamable HTTP 直连（推荐）

对于支持 HTTP 协议传输的 MCP 客户端（如 Cursor、Cline），直接配置端点地址：

```json
{
  "mcpServers": {
    "compilegateway": {
      "url": "http://127.0.0.1:3994/mcp"
    }
  }
}
```

若通过局域网/公网连接启用了鉴权的 Daemon，在请求头中附带 HTTP Basic 凭据：

```json
{
  "mcpServers": {
    "compilegateway": {
      "url": "http://192.168.1.20:3994/mcp",
      "headers": {
        "Authorization": "Basic YWxpY2U6c2VjcmV0"
      }
    }
  }
}
```

> [!NOTE]
> 当在 WAN 模式下启用鉴权时，MCP 对 HTTP Basic 凭据实施基于客户端 IP 的防爆破限流保护。连续密码尝试失败将触发冷却退避，并返回 HTTP 429 与 JSON-RPC 错误码 `-32001` 及 `Retry-After` 响应头。

### Stdio 转发桥接入

对于必须通过 stdio 管道启动的桌面应用（如 Claude Desktop）：

```json
{
  "mcpServers": {
    "compilegateway": {
      "command": "node",
      "args": ["/path/to/CompileGateway/mcp/dist/index.js"]
    }
  }
}
```

转发桥会自动读取共享目录 `CGW_HOME` 中的 `runtime.json` 或 `$CGW_URL` 环境变量以定位正在运行的 Daemon。

## MCP 工具列表 (42 Tools)

所有工具参数 Schema 直接由 Rust 核心结构体通过 `schemars` 自动导出，保证 MCP 工具契约与底层的 REST API 绝对一致。

### 系统与配置

| 工具名 | 用途说明 |
|---|---|
| `cgw_status` | 查看 Daemon 状态、版本、监听范围、语言、访问地址与 MCP 状态 |
| `cgw_get_config` | 读取当前完整系统配置 |
| `cgw_set_config` | 修改配置（`mcp.*`、`allowedRoots`、`hostLock`、`loginIntervalSeconds` 等） |

### 客户机与环境探测

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_clients` | 列出已登记的 SSH 目标机（客户机）列表（包含主机参数、基准目录与 `hostLock` 状态） |
| `cgw_save_client` | 新增或编辑客户机连接配置（支持密钥、密码认证、基准目录与单机独占 `hostLock` 开关） |
| `cgw_delete_client` | 删除客户机配置 |
| `cgw_test_client` | 执行真实 SSH 连接测试并核对 TOFU 主机指纹 |
| `cgw_detect_env` | 执行单次往返远程环境探测（检测 Xcode 多版本/License、JDK、Gradle、Rust 等） |

### 源码目录与文件系统

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_projects` | 列出项目登记及当前运行状态 |
| `cgw_browse` | 浏览宿主机受控根目录（`allowedRoots`）下的子目录，供挑选源码路径 |
| `cgw_create_project` | 登记宿主机源码目录为项目并绑定客户机 |
| `cgw_delete_project` | 移除项目登记（保留所有源码与产物文件） |
| `cgw_tree` | 受控遍历项目远端（`remote`）或宿主机侧（`host`）文件树 |

### 任务管理与执行

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_tasks` | 列出项目的全部触发任务（涵盖 7 种原子类型） |
| `cgw_save_task` | 保存/更新项目任务（校验相对路径与输入输出契约） |
| `cgw_delete_task` | 删除指定任务（自动清理批量工作流中的无效引用） |
| `cgw_preview_task` | 预渲染构建命令与展开路径（Dry-run 校验，不实际执行） |
| `cgw_run_task` | 触发执行指定任务（支持 `enqueue: true` 排队；排队计划持久化落盘，Daemon 重启自动恢复执行），立即返回 `jobId` |

### 批量流水线与跨项目模板

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_batches` | 列出项目内的所有批量工作流 |
| `cgw_save_batch` | 保存批量工作流（校验步骤 ID、前后依赖；支持消费任务输出与 Manifest 命名步骤 `outputs` 声明） |
| `cgw_delete_batch` | 删除批量工作流 |
| `cgw_run_batch` | 触发执行批量工作流（支持 `enqueue: true` 排队；排队计划持久化落盘，Daemon 重启自动恢复执行） |
| `cgw_list_workflows` | 列出跨项目工作流模板（`workflows/*.json`） |
| `cgw_read_workflow` | 读取工作流模板原始 JSON 与解析结构 |
| `cgw_save_workflow` | 新建或更新跨项目工作流模板 |
| `cgw_delete_workflow` | 删除工作流模板 |
| `cgw_apply_workflow` | 实例化应用工作流模板至指定项目（支持动态变量传参及冲突 Diff 预检） |

### 构建类型 (Manifests)

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_manifests` | 列出所有构建类型及其合法性校验状态与默认产物 |
| `cgw_manifest_raw` | 读取指定构建类型的原始 JSON 定义 |
| `cgw_save_manifest` | 保存构建类型 Manifest（支持命名步骤 `outputs` 声明，跑完自动校验类型与存在性） |
| `cgw_delete_manifest` | 删除构建类型 Manifest |
| `cgw_reload_manifests`| 触发重读磁盘 `manifest/` 目录并报告文件变更 |
| `cgw_manifest_status` | 查看构建类型目录状态及缺失的预设模板 |
| `cgw_export_manifest` | 导出构建类型 JSON 文本内容 |
| `cgw_detect_manifests`| 按宿主机目录结构自动探测匹配的构建类型 |

### Job 执行与日志审计

| 工具名 | 用途说明 |
|---|---|
| `cgw_list_jobs` | 查询最近 50 条 Job 执行记录与状态 |
| `cgw_job` | 查询指定 Job 详情、每一步执行结果与产物信息 |
| `cgw_job_logs` | 获取或流式订阅指定 Job 的实时控制台输出 |
| `cgw_cancel_job` | 协作式取消正在运行或排队中的 Job |
| `cgw_retry_job` | 从失败步骤断点重试或重新开始整个 Job |
| `cgw_log_days` | 查看落盘日志归档日期及保留天数 |
| `cgw_logs` | 按日期、来源（webui/cli/mcp/daemon）及作用域检索历史日志 |

## MCP 核心资源 (8 Resources)

| URI | 资源名称 | 内容说明 |
|---|---|---|
| `cgw://guide` | 使用手册 Playbook | 一页式核心操作指南，包含典型编排流程、错误码对照表与最佳实践 |
| `cgw://cli` | 命令行全量对照 | 机器可读格式的 CLI 命令全量对照表、退出码契约与选项 |
| `cgw://schema/manifest` | Manifest Schema | 构建类型 Manifest 的 Draft-07 JSON Schema |
| `cgw://schema/task` | Task Schema | 项目任务的 Draft-07 JSON Schema |
| `cgw://schema/batch` | Batch Schema | 批量工作流的 Draft-07 JSON Schema |
| `cgw://schema/client` | Client Schema | 客户机连接请求体的 Draft-07 JSON Schema |
| `cgw://schema/project` | Project Schema | 项目登记请求体的 Draft-07 JSON Schema |
| `cgw://schema/workflow`| Workflow Schema | 跨项目工作流模板的 Draft-07 JSON Schema |

## Prompt 引导模板 (3 Prompts)

- **`first-build`**：首次构建向导 —— 引导 Agent 依次完成客户机测试、环境检测、项目登记、源码推送与首次构建。
- **`diagnose-job`**：任务诊断向导 —— 接收 `jobId`，自动拉取步骤分解与日志输出，分析失败原因并给出修复或重试建议。
- **`pick-manifest`**：构建类型匹配 —— 执行目录特征探测并协助配置工程变量与产物定位。

## Agent 最佳实践与规约

1. **优先阅读 `cgw://guide`**：在编排复杂构建之前先读取 Playbook 掌握项目锁和并发规则。
2. **合理处理 `409 projectBusy`**：当项目已有 Job 在运行时，不要高频死循环轮询。使用 `enqueue: true` 参与排队，排队计划持久化落盘至 `queued_runs.json`，Daemon 重启自动恢复执行。
3. **识别 `409 hostBusy`**：当开启全局 `hostLock` 或单机开启 `hostLock` 时，多项目共享同一台主机将串行化排队执行；立即请求冲突时返回 409 `hostBusy`。
4. **构建前充分预览**：在执行大型构建前先调用 `cgw_preview_task` 确认拼接出的 Shell 命令与转义路径。
5. **善用断点重试**：流水线失败时使用 `cgw_retry_job` 从失败步骤继续，避免浪费时间重复执行已成功的步骤。
6. **利用步骤级产物**：构建 Manifest 支持在命名步骤中声明 `outputs`，校验通过后自动发布，后续批量步骤可通过 `{{steps.<stepId>.outputs.<name>}}` 消费。

← [首页](Home.zh-cn)
