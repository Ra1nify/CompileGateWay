# 核心概念

CompileGateway 把一份本地源码树推送到 SSH 目标机,在那边完成构建,再把产物取回来或打包导出。

## 这个工具做什么

源码留在开发机,编译器跑在目标机上(通常是 macOS 构建机,Linux 主机同样可以)。一个 daemon 负责把整条链路串起来:

- **推送** —— 把本地目录打包,经 SSH 流进项目的远端目录。
- **构建** —— 在目标机上跑一份构建类型 manifest,输出按行实时回传。
- **拉取 / 导出** —— 把产出的产物拷回宿主机,或者打包成 zip 放进宿主侧的 `exports/` 目录。

daemon 自己不做任何编译。它通过 SSH 跟目标机说话,维护项目、任务、批量工作流、job 历史和日志,并通过 HTTP 对外服务。Windows / Linux / macOS 同一套代码。

## 一个核心,四个前端

所有能力只在 daemon 里实现一份,再由四个前端暴露出来,它们都走同一个 HTTP 接口:

| 前端 | 是什么 |
|---|---|
| WebUI | daemon 提供的浏览器界面(编译期就嵌进二进制里) |
| `cgw`(CLI) | 瘦 HTTP 客户端,永不交互,适合脚本与 agent;裸敲 `cgw` 进入交互环境 |
| MCP | daemon 内建的 MCP 服务,跑在**另一个端口**上(Streamable HTTP,无状态) |
| HTTP 接口 | 上面三个共同使用的接口,也可以直接调用 |

**对等原则：WebUI 能做的，CLI 一定能做。** 反向则根据使用场景做了适度收敛 —— 底层能力（如直接读取六份 JSON Schema）主要服务于脚本与 Agent，界面上保持专注与清爽：

- **JSON Schema** —— `cgw schema [名称]` 与 `GET /api/schema/{名称}` 提供 `manifest`、`task`、`batch`、`client`、`project`、`workflow` 六份。
- **跨项目工作流模板的 apply** —— `cgw workflows apply <项目> <模板>` 以及对应的 apply 接口。实例化后即在项目中生成标准任务与批量工作流。

## 对象模型一瞥

```
客户机 (Client / SSH 目标机)
  └─ 项目 (宿主机源码目录 + 远端镜像目录)
       ├─ 任务 Task (静态动作配置: build / push / pull / copy / move / delete / export)
       ├─ 批量工作流 Batch (项目内任务的有序执行列表)
       └─ 工作流模板 Workflow (跨项目可复用模板，实例化为 Task + Batch)
            └─ 执行记录 Job (一次实际执行实例: 包含步骤结果、快照与日志)
                 └─ 日志
```

## 客户机 (Client)

一台客户机就是一台受控的 SSH 远程构建机。

- 字段：`id`、`name`、`host`、`port`（缺省 `22`）、`auth`（`method: password | key`、`username`、`password`、`keyPath`、`passphrase`）、`tags`、`baseDir`（缺省 `~/CompileGateway`）、`baseDirResolved`、`hostKey`、`hostKeyLine`。
- 密码与口令采用**只写模式（Write-only）**：保存后接口绝不回传密码明文，仅返回配置状态标志，杜绝凭据泄漏风险。
- 主机指纹走 TOFU（初次使用信任）—— 首次连接时记录指纹，后续若发生指纹变动则明确报错并等待用户显式确认。
- 基准目录采用**实时验证机制**：保存时服务端会实时通过 SSH 执行 `mkdir -p` 并在远端解析绝对路径，确认具有读写权限后方持久化保存。

## 项目

项目把 daemon 这台机器上的一个源码目录,和某台客户机上的一个远端目录绑在一起。

- 远端工作在 `<baseDir>/<slug>` 下进行;宿主侧按同样的 slug 约定镜像。
- 项目拥有自己的 `tasks` 和 `batches`、自己的 `excludes`(默认 `.git`、`.DS_Store`、`Thumbs.db`),以及最近一次同步/产物的摘要。
- 项目登记是权威来源:任务与批量工作流存在 `projects.json` 里,不在 daemon 的内存里。

## 任务

任务是挂在项目上的**一个动作**。

- 字段:`id`、`name`、`kind`、`variables`、`signingMode`、`artifactPath`、`artifacts`、`xcodePath`、`clean`、`keepCrlf`、`overwrite`、`inputs`、`outputs`。
- `artifacts` 列表非空时会覆盖 `artifactPath`。
- 用 `cgw run <项目> <任务id>` 执行,它立刻返回一个 job;加 `--wait` 会跟到结束。
- `cgw tasks preview <项目> <任务id>` 只渲染命令与展开后的路径,不跑任何东西。

## 批量工作流

批量工作流是一串有序步骤,每步 `{ taskId, id?, variables? }`。

- 步骤 `id` 可以省略,但后续步骤要引用它的输出时**必须**写。
- 失败即停:任意一步失败就终止整条,后续步骤记为 skipped。
- 步骤间的输出用 `{{steps.<步骤 id>.outputs.<名称>}}` 传递,写进后续步骤的 `variables`;产出那一步必须更早,并且要声明过该输出。
- 因为失败即停,汇总的 `files` / `bytes` 是**已执行步骤**的合计。

## 工作流模板

工作流模板是跨项目复用的模板:`name`、`description`、`variables`,以及内联的 `tasks` 与 `steps`。

- 随程序自带两份,只在**首次**铺一次(`push-build-pull`、`build-export-zip`);`cgw workflows refresh` 只补回缺失的那几份。
- `{{名字}}` 替换只替换**声明过的**变量;传了没声明的参数会被直接拒绝。
- apply 是**预览优先**:不带 `apply` 时返回会多出哪些任务、那个确定性的 batch id(`wf-<模板 id>`)、以及冲突清单。冲突(任务 id 或 batch id 撞车)是错误(`workflowConflict`),除非显式 `overwrite`。
- apply 会保留模板里的任务 id,而且是**单向**的:改模板不影响已经 apply 过的项目。apply 之后它就是普通任务 + 一条批量工作流。

## job

一个 job 就是**一次执行** —— 一次任务、一次批量、一次推送或一次拉取。

- 它记下自己的 kind、标题、来源(`webui` / `cli` / `mcp` / `daemon`)、每一步的记录、产物、文件数与字节数,以及**本次执行用到的输入快照**。
- 每一步带自己的状态、消息、输入与输出(写成 `host:` / `remote:` 字符串)以及错误码。
- 历史保留最近 50 条。

## 日志

日志是"发生了什么"的逐行记录。

- 每一行都带**来源**(`webui` / `cli` / `mcp` / `daemon`)和**作用域**(全局、客户机、项目、job),所以可以按需筛选。MCP 驱动的操作标成 `mcp`,一眼就能看出 AI 做了什么。
- 文件按天滚动为 `logs/YYYY-MM-DD.log`,保留天数由 `logRetainDays` 决定。
- 实时读:WebUI 的「日志」页;或者 `cgw logs client <id>`(实时)、`cgw logs`(按天)、`cgw logs <YYYY-MM-DD>`(历史)。

## 七种任务类型

| kind | 必需字段 | 作用 |
|---|---|---|
| `build` | `manifest` | 在目标机上跑一份构建类型 manifest。它**不会**先推送。 |
| `push` | — | 宿主机 → 远端的源码同步;带 shebang 的脚本默认做 CRLF→LF 归一化,`keepCrlf` 可关掉。 |
| `pull` | `sources`、`to` | 远端 → 宿主机,把产物搬回来。 |
| `copy` | `from`、`to` | 复制文件或目录;任意同侧/跨侧组合。 |
| `move` | `from`、`to` | 移动;跨侧退化为复制后删源。 |
| `delete` | `target` | 删除文件或目录。 |
| `export` | `sources` | 打包成 zip 放进宿主侧 `exports/`;`outputName` 指定文件名。 |

## job 生命周期

```
pending ──▶ running ──▶ ok | failed | cancelled
```

- `pending` 表示已排队、尚未开始;`running` 表示正在执行;`ok`、`failed`、`cancelled` 是三个终态,取消是协作式的。
- 一个 job 有若干**步骤**。单个任务是"一步";批量工作流是"每个任务一步",编号按它在批量里的位置来。
- job 会列出产出的**产物**,以及本次执行的文件数与字节数。
- **重试从指定步骤开始**(默认从失败的那一步),已经成功的步骤不会重跑。重试产生的是**新 job**,只登记剩余步骤,但步骤序号仍与原批量对齐。
- **job 记录了自己用过的输入**,重试用的就是那份快照 —— 不是任务当前的取值。事后改任务,不会改变旧 job 重试的结果。

## 并发:项目、队列与主机

- **同一个项目同一时间只有一个 job。** 第二个请求会被拒绝,返回 409 `projectBusy`,并带上占位者的 `jobId`。
- **加 `enqueue: true`(MCP)/ `--enqueue`(CLI)** 则不报错,而是排进该项目的 **FIFO** 队列。项目一空出来,队首自动开跑;排队中的 job 仍可取消。
- **排队中的 job 不会在 daemon 重启后恢复**,它们会被标成 `cancelled` 并写明"请重新提交"。daemon 停下时仍在 `running` 的 job 同样不会续跑,会被记为失败并标注"任务中断"。
- **`hostLock`**(配置项,默认**关**):打开后是主机级互斥 —— 共用同一台客户机的所有项目,一次只允许一个 job 在跑。被挡住的立即请求返回 409 `hostBusy`(带 `clientId`);排队的工作则会等。
- 开/关 `hostLock` 立即生效。

## 输入 / 输出契约

取值来自任务的 `variables`(缺失时回退到声明里的 `default`)。`inputs` 是一张 `"<名称>": { type, default?, side?, values? }` 的表,`type` 取 `string`(默认)、`enum`、`path`、`bool`、`number`。声明一个 input 就是对这个任务"接受什么"的承诺,渲染命令之前会先校验。

其它字段里用 `{{inputs.<名称>}}` 引用它。路径在展开**之后**会再校验一次,所以无论 `..` 和越界是怎么拼出来的都会被拒。`cgw tasks preview` 用同一套展开逻辑渲染但不执行,是查看"这组取值会变成什么"最省事的办法。

声明的 `outputs` 是 `{ type: file | directory, side, path, required? }`。命令退出后,daemon 会在指定的一侧核对存在性与类型。**必需**的输出缺失会让这一步以错误码 `artifactMissing` 失败 —— 退出码 0 本身不再等于成功。写成 `required: false` 则只记一条备注、不算失败,可选产物就该这么写。

## 东西放在哪、归谁管

| 路径 | 归属 | 内容 |
|---|---|---|
| `projects.json` | daemon(运行时状态) | 项目及其 `tasks`、`batches` 的**权威来源**。 |
| `jobs.json` | daemon(运行时状态) | job 历史,最近 50 条,重启后恢复。 |
| `manifest/` | **你** | 构建类型,一个类型一份 `<id>.json`。随程序模板只铺一次。格式见 `cgw schema manifest`。 |
| `workflows/` | **你** | 跨项目工作流模板,`<id>.json`。同一条铺法:自带模板只在首次复制进来,`cgw workflows refresh` 只补缺失的几份。 |
| `config.json` | 首次运行时生成 | daemon 配置:端口、语言、监听模式、鉴权、MCP、`allowedRoots`、`hostLock`、`logRetainDays`。 |
| `runtime.json` | daemon | 实际绑定的端口(`url`、`mcpUrl`、`mcpPort` 等),让瘦客户端能找到自动挑端口的 daemon。 |
| `logs/` | daemon | 按天的日志文件 `YYYY-MM-DD.log`,按 `logRetainDays` 清理。 |
| `exports/` | daemon | 宿主侧存放导出 zip 的目录。 |

标着**你**的两个目录,当成手写内容来对待:daemon 只铺一次,之后绝不覆盖你改过的东西。

## 架构定位与边界说明

- **关于 OpenWrt 路由器字面原生插件 (如 luci-app-cgw)**：
  **当前阶段不计划支持**。编译网关的核心定位是源码与构建任务的分发调度网关，而普通路由器设备硬件资源（CPU、RAM 与 Flash）高度受限，通常不是编译机本体（构建典型固件动辄需要 16GB+ 内存与数十 GB 磁盘空间）。CompileGateway Daemon 与源码建议运行在主力开发机或专用 Linux/NAS 构建机上；路由器更适合作为受控部署目标端。后续若有轻量路由器状态监控或固件一键推送等具体场景诉求，再评估专用的极轻量接入形态。

## 接下来读什么

- [快速开始](Getting-Started.zh-cn) —— 安装、第一次运行、第一次构建。
- [客户机与环境](Clients-and-Environment.zh-cn) —— SSH 目标机、主机指纹、基准目录、环境检测。
- [任务](Tasks.zh-cn) —— 七种任务与全部字段,带示例。
- [批量与工作流](Batches-and-Workflows.zh-cn) —— 步骤顺序、步骤间变量、重试、模板。
- [构建类型](Manifests.zh-cn) —— manifest、变量、产物列表、类型探测。
- [Web 界面](WebUI.zh-cn) —— 浏览器界面上有什么。
- [命令行](CLI.zh-cn) —— 完整命令面与退出码。
- [MCP](MCP.zh-cn) —— AI agent 看到的工具集。
- [HTTP 接口](HTTP-API.zh-cn) —— 端点、错误码、异步模式。
- [配置与日志](Configuration-and-Logs.zh-cn) —— `config.json`、端口、保留策略、日志读取。
- [疑难排查](Troubleshooting.zh-cn) —— 那些常见的意外。

← [首页](Home.zh-cn)
