# 批量工作流与跨项目模板

批量工作流（Batch）是单个项目内的有序执行流水线；跨项目工作流模板（Workflow Preset）则是可复用的流程模板，用于将批量工作流及其依赖的任务一键实例化到项目中。

> [!NOTE] 核心概念区分：Task、Batch 与 Job
> - **Task（任务定义）**：静态配置，定义单个原子操作（如推送源码、执行编译、拉取产物、打包归档）。
> - **Batch（批量流水线）**：静态编排，定义多个 Task 的顺序依赖与变量流转规则。
> - **Job（运行时任务）**：Task 或 Batch 执行时的**动态实例**（带有唯一 `jobId`）。执行日志、每步耗时、产物快照均记录在 Job 中。修改 Task 或 Batch 的定义绝不会影响已运行完成的历史 Job。

## 什么是批量工作流

批量工作流归属于某一个具体的项目，包含从上到下按序执行的步骤数组。每个步骤为一个对象：

```json
{ "taskId": "t-build", "id": "build", "variables": { "scheme": "Runner" } }
```

- `taskId`（必填）—— **同一个**项目中的任务 ID。空 `taskId` 在保存时会被拒绝。
- `id`（可选）—— 步骤名称。当后续步骤需要引用本步骤的产物输出时**必填**。
- `variables`（可选）—— 仅对本步骤生效的局部变量覆盖。

保存在项目中的批量工作流格式如下：

```json
{
  "id": "b-8f2c1a9d0e44", "name": "first build",
  "steps": [
    { "taskId": "t-push-source", "id": "push" },
    { "taskId": "t-build-app", "id": "build" },
    { "taskId": "t-pull-app", "id": "pull",
      "variables": { "source": "{{steps.build.outputs.app}}" } }
  ]
}
```

批量工作流的 ID 与名称均可自定义；若通过 API 创建时未指定 `id`，Daemon 会自动分配一个 `b-<hex>` 格式的 ID。在接受 Batch ID 的地方通常也可以直接传入 Batch 名称。使用以下命令进行管理：

```bash
cgw batches p-ios                                  # 列出项目的所有批量工作流
cgw batch save p-ios "first build" t-push-source t-build-app t-pull-app
cgw batch save p-ios --file batch.json             # 步骤需要局部变量或指定 id 时
cgw batch run p-ios "first build" --wait           # 执行并等待至结束
cgw batch rm p-ios b-8f2c1a9d0e44                  # 删除
```

对应 HTTP 接口为 `GET|POST /api/projects/{id}/batches`、`PATCH|DELETE /api/projects/{id}/batches/{batchId}` 以及 `POST /api/projects/{id}/batches/{batchId}/run`（或 `/enqueue`）。步骤数组为空时拒绝保存。

## 步骤 ID 与输出引用

步骤 `id` 虽然是可选的，但若后续步骤想要消费当前步骤的产出，则必须为产生步骤指定 `id`。Daemon 会在校验产物后将输出保存在对应步骤 ID 下，后续步骤在 `variables` 中通过 `{{steps.<步骤id>.outputs.<输出名>}}` 进行引用：

```json
{ "taskId": "t-pull-app", "id": "pull",
  "variables": { "source": "{{steps.build.outputs.app}}" } }
```

相关校验在**保存批量工作流时**静态完成，而非等到运行时才暴露：

- 步骤 ID 在当前 Batch 内部必须唯一；
- 被引用的步骤必须存在且必须排在**当前步骤之前**（不能自引用或引用后置步骤）；
- 产出端必须显式声明了该输出项（支持任务定义中的 `outputs` 契约，或构建 Manifest 中命名步骤的 `outputs` 声明）；
- 若消费端输入声明了 `side`（`host` 或 `remote`），必须与产出端的 `side` 匹配。

引用会解析为前序步骤实际校验成功的真实路径。若前序步骤产物缺失，前序步骤本身直接判为失败，而不会将未解析的字符串直接透传给后续步骤（详见 [任务](Tasks.zh-cn) 的 `inputs` / `outputs` 契约）。

## 步骤级变量覆盖

步骤中的 `variables` 仅在执行该步骤时临时覆盖任务本身的变量定义。项目 JSON 中任务的原始定义不会被改写，后续步骤重新从任务默认值起算。这既可以用来“在流水线中指定特定 scheme”，也可以用于“将上一步的产物路径作为下一步的输入”。步骤变量覆盖会记录在 Job 的输入快照中，确保重试时准确复现当时执行的参数。

## 失败熔断、跳过与断点重试

批量工作流采用快速失败（Fail-Fast）策略。首个失败的步骤会立即中断整条流水线；后续所有未执行的步骤均记录为 `skipped`，Job 错误消息会明确指出失败的步骤（如 `第 2 步失败 - ...`）。已执行步骤的文件数与字节数会被累加，取消 Batch 时未完成的步骤也会标记为 `skipped` 并将 Job 置为 `cancelled`。

执行失败的 Batch 会记录 `retryFromStep` 索引（首个未成功步骤的下标，0 开始）：

```bash
cgw jobs retry j-3c1f0a7b2e                # 从首个未完成步骤断点重试
cgw jobs retry j-3c1f0a7b2e --from 1       # 从指定步骤下标重试（0-based）
cgw jobs retry j-3c1f0a7b2e --from-start   # 从第 1 步重新开始
cgw jobs retry j-3c1f0a7b2e --wait         # 跟踪重试直至结束
```

对应 HTTP 接口为 `POST /api/jobs/{id}/retry`，请求体可带 `{ "fromStep": 1 }` 或 `{ "fromStart": true }`。

重试会创建一个**新的 Job**。新 Job 仅注册剩余待执行的步骤，但步骤编号与原始 Batch 保持对齐，方便两份记录对照。同时，重试复用原 Job 中保存的输入快照，而不是重新读取任务当前的变量值，防止在此期间修改任务导致已诊断的参数被静默替换。

## 项目锁与排队机制

单个项目同一时刻只允许一个 Job 运行，整个 Batch 在执行期间（从第一步到最后一步）持续持有项目级锁。在锁被占用时，后续普通执行请求会直接返回 HTTP 409 `projectBusy` 并携带当前持有者的 `jobId`（CLI 退出码为 `4`）。

若希望排队等待而非直接报错，可使用排队模式：

```bash
cgw batch run p-ios "first build" --enqueue
cgw run p-ios t-push-source --enqueue
```

对应 HTTP 接口为 `POST /api/projects/{id}/tasks/{taskId}/enqueue` 与 `POST /api/projects/{id}/batches/{batchId}/enqueue`；MCP 在 `cgw_run_task` / `cgw_run_batch` 中提供 `enqueue: true` 参数。注意 `cgw workflows apply ... --run` 走的是立即执行路径，遇到忙碌项目仍会返回 `projectBusy`。

排队任务的运行特性：
- 初始状态为 `pending`，加入单项目 FIFO 队列（创建时返回 `queuePosition`）；
- 当项目空闲时自动提拔执行，无需人工重新提交；在 `pending` 状态下可被随时取消；
- **Daemon 重启后自动恢复排队**：执行计划落盘保存至 `queued_runs.json`，Daemon 启动时自动恢复排队中的 Pending Job 并重新唤醒队列消费（无持久化计划的孤儿记录自动平滑标记为已取消）。

可选的主机互斥（默认关闭）支持全局配置 `hostLock` 与单台客户机细粒度 `hostLock`，开启后共用同一台物理客户机的项目将严格串行排队执行，立即执行冲突时返回 409 `hostBusy`。详见 [配置与日志](Configuration-and-Logs.zh-cn) 与 [客户机与环境](Clients-and-Environment.zh-cn)。

## 跨项目工作流模板

任务与 Batch 解决的是**项目内**的复用。当需要将同一套成熟流水线复用到新项目时，使用跨项目工作流模板。模板以独立 JSON 文件形式保存在与 `config.json` 同级的目录下：

```
<config.json 同目录>/workflows/<id>.json
```

模板目录在首次初始化时播种一次内置模板，之后 Daemon 绝不会主动修改或覆盖该目录。`cgw workflows` 每次调用都会直接重读磁盘文件，因此在外部编辑模板无需额外刷新。

### 内置模板

- **`push-build-pull`** —— “首次构建：推送 → 构建 → 回传”。推荐作为新项目的首次运行流水线，解决 `build` 任务不自动 `push` 的前置依赖。
- **`push-build-export`** —— “端到端导出：推送 → 构建 → 直接打包 ZIP 导出”。一键完成源码推送、远程编译并将产物打包为 ZIP 存入本地 `exports/`。
- **`build-export-zip`** —— “构建 → 打包导出”。适合代码已同步、直接触发构建并产出 ZIP 归档包。
- **`clean-push-build-pull`** —— “纯净重构：全量覆写推送 → 纯净构建 → 回传”。用于清理远端脏缓存与孤立文件。

## 模板文件格式

```json
{
  "name": "My pipeline",
  "description": "push, build and pull the app",
  "variables": [
    { "name": "manifest", "label": "Build type", "required": true, "default": "xcode" },
    { "name": "scheme", "label": "Xcode scheme", "required": false, "default": "App" }
  ],
  "tasks": [
    { "id": "t-push", "kind": "push", "name": "Push source" },
    { "id": "t-build", "kind": "build", "name": "Build {{scheme}}",
      "manifest": "{{manifest}}", "variables": { "scheme": "{{scheme}}" },
      "artifactPath": "build/{{scheme}}.app",
      "outputs": { "app": { "type": "directory", "side": "remote", "path": "build/{{scheme}}.app" } } },
    { "id": "t-pull", "kind": "pull", "name": "Pull artifact",
      "sources": [ { "side": "remote", "path": "{{inputs.source}}" } ],
      "to": { "side": "host", "path": "artifacts" },
      "inputs": { "source": { "type": "path", "side": "remote" } } }
  ],
  "steps": [
    { "id": "push", "taskId": "t-push" },
    { "id": "build", "taskId": "t-build" },
    { "id": "pull", "taskId": "t-pull", "variables": { "source": "{{steps.build.outputs.app}}" } }
  ]
}
```

- `name`（必填且非空）—— 显示名称，同时作为默认 Batch 名称；`description` 为说明文本。
- `variables` —— 参数声明（`name`, `label`, `required`, `default`），参数名必须唯一。缺少默认值的必填参数在 Apply 时若未传参则报错。
- `tasks` —— 内联任务定义数组，格式与标准任务 API 一致（详见 [任务](Tasks.zh-cn)）。每个任务需指定非空的唯一 `id`。
- `steps` —— Batch 步骤列表（至少包含一个步骤）。

变量替换采用单次遍历，仅替换模板中**显式声明**的变量。未声明的占位符（如 `{{configuration}}` 或 `{{inputs.source}}`）会原样保留给下层 Manifest 或任务契约，互不干扰。传入未声明的参数会直接报错（`workflowInvalid`），防止因拼写错误导致静默失效。

## 实例化应用 (Apply)

```bash
cgw workflows                                              # 列出模板及其有效性
cgw workflows show push-build-pull                         # 查看原始 JSON 与解析结果
cgw workflows apply p-ios push-build-pull --set scheme=Runner   # 默认仅预览
cgw workflows apply p-ios push-build-pull --set scheme=Runner --apply
cgw workflows apply p-ios push-build-pull --apply --overwrite --run
```

Apply 遵循**预览优先**原则。未加 `--apply` 时不会写入任何数据，仅返回 `{ ok, applied: false, preview, conflicts }`：

```json
{ "ok": true, "applied": false, "conflicts": [],
  "preview": { "presetId": "push-build-pull", "batchId": "wf-push-build-pull",
    "batchName": "首次构建:推送 → 构建 → 回传",
    "tasks": [ { "id": "t-push-source", "kind": "push", "name": "推送源码" } ],
    "steps": [ { "id": "push", "taskId": "t-push-source" } ], "conflicts": [] } }
```

实例化生成的 Batch ID 为确定性的 `wf-<preset id>`，因此对同一项目多次 Apply 相同模板会覆盖落入同一个 Batch，避免数量膨胀。同名冲突指目标项目中已存在相同的任务 ID 或 Batch ID。当存在冲突且未指定 `--overwrite` 时，请求会以 HTTP 409 `workflowConflict` 拒绝写入；带上 `--overwrite` 则会在原位置原地替换同名任务或 Batch。

加 `--run` 会在写入后立刻触发该 Batch 并返回 `jobId`。写入后的产物就是普通的任务与 Batch，可在 WebUI 或 CLI 中直接使用：

```bash
cgw batch run p-ios wf-push-build-pull
```

对应 HTTP 接口为 `POST /api/projects/{id}/workflows/{preset}/apply`。

## 模板管理与生命周期

模板是用户资产，可通过 CLI、API 或直接编辑文件进行管理。保存时会严格校验，结构非法直接返回 400 `workflowInvalid`：

```bash
cgw workflows save my-flow --file my-flow.json
cgw workflows save my-flow --body '<json>'
cgw workflows rm my-flow                           # 删除模板（不影响已 Apply 的项目）
cgw workflows refresh                              # 仅补齐缺失的内置模板
```

获取 Schema：

```bash
cgw schema workflow                                # JSON Schema (draft-07)
```

项目 JSON 是数据的唯一权威来源。Apply 是单向的（模板 → 项目），修改或删除模板绝不会影响已经实例化的项目。

← [首页](Home.zh-cn)
