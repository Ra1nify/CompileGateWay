# 任务

任务是挂在项目上的**一个动作** —— 跑一份构建类型、在宿主机与 SSH 目标机之间搬文件、或者打包 zip —— 也是你执行、预览、重试,以及在批量工作流里引用的单位。

## 任务是什么

一个项目拥有一组任务。每个任务只有一个 `kind`,一共七种:`build`、`push`、`pull`、`copy`、`move`、`delete`、`export`。

七种类型**共用同一个扁平 JSON 结构**:你只填自己这种类型用得上的字段,别的字段会被忽略。保存时 `Task::validate()` 会跑一遍,结构不对直接返回 `taskInvalid`,而不是跑到一半才失败。`kind` 留空或不写等于 `build`,所以 `{"manifest": "xcode"}` 本身就是一份合法任务体;其余类型必须写明 `kind`,因为只凭 `from` + `to` 分不清是 `copy` 还是 `move`。

**任务 id 由服务端生成。** 不需要你自己编:POST 一个不带 `id` 的请求体,daemon 会分配一个。

```
POST /api/projects/{项目id}/tasks        请求体:任务 JSON        ->  { ok, project }
```

新任务会出现在返回的 `project.tasks` 里,id 形如 `t-` + 12 位十六进制(`t-9f3ac41b7e02`),取自系统随机源。要修改已有任务,就把这个 `id` **带回去**再 POST 同一份请求体,daemon 就地替换那一条;`id` 在项目里找不到对应任务时会新建一条。命令行侧 `cgw tasks save <项目> --body '<json>' | --file <路径> | -` 与 WebUI 的任务对话框发的是同一份请求体。任务存在 `projects.json` 里,它才是权威来源,不是 daemon 的内存。

## 字段参考

| 字段 | 类型 | 用到的类型 | 含义 |
|---|---|---|---|
| `id` | string | 全部 | 服务端生成(`t-` + 12 位十六进制)。带回来就是修改那一条,不带走就是新建。 |
| `kind` | string | 全部 | 七种之一。留空 = `build`。不认识的值会被拒。 |
| `name` | string | 全部 | 显示名。为空时退回 manifest id,再退回 kind。 |
| `manifest` | string | `build` | 构建类型 id;`manifest/<id>.json` 必须存在且能解析,否则保存返回 `manifestInvalid`。 |
| `variables` | object | `build`,以及声明了 `inputs` 的类型 | 名字 → 值:`build` 是构建变量,声明了输入的任务是输入取值,批量里还可以做单步覆盖。 |
| `signingMode` | string | `build` | `signed` 或 `unsigned`;以保留变量 `{{signingExtraArgs}}` 的形式注入 manifest。不是 `signed` 的一律存成 `unsigned`,这也是默认值。 |
| `artifactPath` | string | `build` | 单个产物路径模板。被非空的 `artifacts` 覆盖。 |
| `artifacts` | string[] | `build` | 产物路径列表。非空时以它为准。 |
| `xcodePath` | string | `build` | `.app` 路径,按次作为 `DEVELOPER_DIR` 传入。留空 = 用目标机的全局 `xcode-select`。 |
| `clean` | bool | `push` | 解包前先清空远端项目目录。默认 false。 |
| `keepCrlf` | bool | `push` | 按原样推字节,**不**做 CRLF → LF 归一化。默认 false(即做归一化)。 |
| `overwrite` | bool | `copy`、`move`、`pull` | 允许覆盖已存在的目标。默认 false。 |
| `from` | Location | `copy`、`move` | 来源 `{ side, path }`。 |
| `to` | Location | `copy`、`move`、`pull` | 目标。它**就是完整的目标路径**,所以可以改名;不是"往这个目录里放"。 |
| `target` | Location | `delete` | 要删除的东西。 |
| `sources` | Location[] | `pull`、`export` | 至少一个来源 `{ side, path }`。 |
| `outputName` | string | `export` | zip 文件名。留空 = `<项目名>-<epoch 秒>.zip`。 |
| `inputs` | object | 全部 | 声明的输入:`"<名字>": { type, default?, side?, values? }`。见下文「输入声明」。 |
| `outputs` | object | 全部 | 声明的输出:`"<名字>": { type, side, path, required? }`。见下文「输出声明」。 |

**Location** 永远是 `{ "side": "remote" | "host", "path": "<相对路径>" }`。`side` 缺省表示 `remote`;别的取值会被拒。`remote` 是目标机上的项目根(`<baseDir>/<slug>`),`host` 是 daemon 这台机器上同一 slug 的目录(`<家目录>/CompileGateway/<slug>`)。

每个 `path` 都必须是**相对于该侧项目根**的相对路径。`..`、绝对路径、`~`、盘符、UNC 前缀都在保存阶段就被拒(`taskInvalid`);宿主侧还会再叠一层符号链接检查。

## build

在目标机上跑一份构建类型。必填:`manifest`(而且必须存在)。可选:`variables`、`signingMode`、`artifactPath`、`artifacts`、`xcodePath`、`inputs`、`outputs`。`kind` 可以不写。

> [!NOTE]
> **注意事项**：`build` 任务严格专注于在目标机上执行构建命令，**不会隐式执行源码推送**。新登记的项目在构建前请先执行一次 `push` 任务，或推荐直接使用 `push-build-pull` 批量工作流完成闭环。

产物路径优先级（从高到低）：任务的 `artifacts` > 任务的 `artifactPath` > Manifest 的 `build.artifacts` > Manifest 的 `build.artifactPath`。构建超时时间为 3600 秒。

```json
{
  "kind": "build",
  "manifest": "flutter-ios",
  "variables": { "scheme": "Runner" },
  "signingMode": "unsigned",
  "artifacts": [".cgw/out/Runner.ipa"]
}
```

**两侧:** 构建完全发生在 `remote` 一侧的项目远端根里,所以 `artifactPath`、`artifacts` 和远端输出都是远端路径 —— 要把东西带回宿主机是另一条 `pull`。

## push

把宿主机的源码树复制到远端项目目录。必填:无 —— 来源目录与远端落点都由项目登记提供。可选:`clean`、`keepCrlf`。

`clean: false`(默认)直接解包覆盖;`clean: true` 先删掉远端目录再重建,并拒绝把 `~`、`/`、`.` 或基准目录本身当作目标。`keepCrlf: false`(默认)时,以 `#!` 开头的脚本会被 CRLF → LF 归一化,免得 Windows 上检出的脚本到了目标机报 `bad interpreter: /bin/sh^M`;二进制文件一字节都不动。超时 900 秒,传输走 tar over SSH 且**每次都是全量**。

```json
{ "kind": "push", "name": "推送源码", "clean": false, "keepCrlf": false }
```

**两侧:** host → remote;两个端点都由项目提供,所以任务本身不需要写路径。

## pull

把远端路径搬回宿主机。必填:`sources`(至少一个)、`to`。可选:`overwrite`、`inputs`、`outputs`。

```json
{
  "kind": "pull",
  "sources": [{ "side": "remote", "path": ".cgw/out/Runner.ipa" }],
  "to": { "side": "host", "path": "artifacts" },
  "overwrite": true
}
```

**两侧:** remote → host;`sources` 是远端路径,`to` 是宿主侧落点,各自相对于自己那一侧的项目根。

## copy

复制文件或目录。必填:`from`、`to`。可选:`overwrite`、`inputs`、`outputs`。

同侧复制交给目标机自己的 `cp -a`(宿主侧走 `std::fs`);跨侧复制走 tar over SSH。保存阶段会拒绝"`to` 位于自己 `from` 之内"的同侧写法,因为那种写法运行时必然失败。

```json
{
  "kind": "copy",
  "from": { "side": "remote", "path": ".cgw/out/App.app" },
  "to": { "side": "remote", "path": ".artifacts/staging/App.app" },
  "overwrite": true
}
```

**两侧:** 任意组合 —— host → host、host → remote、remote → host、remote → remote —— 具体走哪条传输路径由两侧决定。

## move

移动文件或目录。必填:`from`、`to`。可选:`overwrite`、`inputs`、`outputs`。规则与"目标嵌在来源之内"的检查都和 `copy` 相同。

```json
{
  "kind": "move",
  "from": { "side": "host", "path": "artifacts/Runner.ipa" },
  "to": { "side": "host", "path": "archive/Runner.ipa" },
  "overwrite": true
}
```

**两侧:** 任意组合,但跨机器没有共同的 `rename` —— 跨侧 move 退化为"复制后删源",因此不是原子操作。

## delete

删除文件或目录。必填:`target`,且它不能是项目根本身。可选:`inputs`、`outputs`。

```json
{ "kind": "delete", "target": { "side": "remote", "path": ".artifacts/staging" } }
```

**两侧:** 两侧都有意义;同侧删除走目标机的 `rm -rf` 或宿主机的文件系统调用。

## export

把一个或多个路径打包成 zip,放进宿主侧的 `exports/` 目录。必填:`sources`(至少一个)。可选:`outputName`、`inputs`、`outputs`。

名字默认 `<项目名>-<epoch 秒>.zip`;压缩包落在 `<宿主机>/exports/`,可以用 `cgw exports <项目>` 列出,也可以在 WebUI 里下载。zip 由内置的小写入器生成:条目权限写死 `0644`、文件名 UTF-8、符号链接会被跳过(只报数字,不打包进去)。`files` / `bytes` 报的是**打包进去的内容**,不是 zip 文件本身的大小。

```json
{ "kind": "export", "sources": [{ "side": "host", "path": "artifacts" }], "outputName": "Runner-release.zip" }
```

**两侧:** `sources` 可以在任意一侧,但产物始终写在宿主机一侧。

## 变量与构建变量

构建类型在自己的 manifest 里声明变量,形式是 `{ name, label, required, default }` 的数组(见[构建类型(manifest)](Manifests.zh-cn));`build` 任务通过自己的 `variables` 映射按变量名给值:

```json
{ "kind": "build", "manifest": "flutter-ios", "variables": { "scheme": "Runner", "configuration": "Release" } }
```

- manifest 构建步骤与产物路径里写成 `{{名字}}` 的占位符会被替换成最终值。普通占位符必须对应一个声明过的变量,而且替换值会做 shell 转义,所以一个带 `;` 的值没法在目标机上执行命令。
- 取值先看任务里的条目,再退回声明的 `default`。如果一个变量标了 `required` 却仍然为空,整次运行会在任何东西发往目标机之前被拒,错误码 `variableRequired`(HTTP 400)。
- `variables` 同时也是声明式输入的取值通道(见下一节),以及批量里单步覆盖的通道。

**保留变量 `signingExtraArgs`** 由 `signingMode` 注入,不在 manifest 里声明 —— 不要把它写进 `variables`,也不要写进 manifest 的变量表。

- `"signingMode": "signed"` → 展开成空字符串。
- `"signingMode": "unsigned"` → 展开成 `CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=`。
- 不是 `signed`(忽略大小写)的一律存成 `unsigned`;字段整个不写也一样。

它展开成**多个裸露参数**,所以和别的占位符不同,它不按"一个值"加引号 —— 这正是它被单独保留的原因。`xcodePath` 也不是变量:daemon 按次把它作为 `export DEVELOPER_DIR=...` 放在所有步骤之前,所以从不修改目标机的全局 `xcode-select`。

## 输入声明

`inputs` 是 `"<名字>": { type, default?, side?, values? }` 的映射。声明一个输入就等于对"这个任务接受什么"作出承诺,它在渲染命令之前就会被校验。

| 键 | 必填 | 含义 |
|---|---|---|
| `type` | 是 | `string`(默认)、`enum`、`path`、`bool`、`number`。用于校验和界面提示。 |
| `default` | 否 | 既没有变量也没有快照时用的值。 |
| `side` | 否 | 只对 `path` 输入有意义:这个路径属于哪一侧(`remote` / `host`)。留空 = 不检查侧。 |
| `values` | 否 | 允许的取值。声明成 `enum` 却没有给 `values` 会在保存阶段被拒。 |

按类型校验:`bool` 接受 `true` / `false` / `1` / `0`;`number` 必须能解析成数字;`values` 非空时取值必须在其中。其余情况是空字符串,这是允许的。

**取值怎么来。** 正常一次运行时,先看任务 `variables` 里同名的条目,再退回声明的 `default`,两者都没有就是空字符串。**重试**时,记录在 job 里的输入快照优先于这两者,所以重试复现的是当时真正跑过的值。

**怎么引用。** 其它字段写 `{{inputs.<名字>}}`。展开作用于 `artifactPath`、`artifacts` 的每一项、`outputName`、该类型真正用到的每个 Location 的 `path`(`from` / `to` / `target` / `sources[]`),以及每个声明输出的 `path`。展开是**单遍**的:替换进去的值里再出现 `{{...}}` 只会原样保留,所以一个取值没法注入另一个占位符。引用一个没有声明的输入,保存阶段会被拒,运行时也会被拒。

**展开后的路径会被重新校验。** 替换之后,每条路径都要过一遍和手写路径完全相同的归一化与边界检查 —— 不许 `..`、不许绝对路径、不许 `~`、不许盘符,展开成项目根本身的取值也会被拒。这件事每次运行都做,所以一个取值没法事后塞进逃逸。失败会以 `inputInvalid` 返回。

## 输出声明

`outputs` 是 `"<名字>": { type, side, path, required? }` 的映射。

| 键 | 必填 | 含义 |
|---|---|---|
| `type` | 否 | `file`(默认)或 `directory`。认不出的取值按 `file` 处理。 |
| `side` | 是 | `remote` 或 `host` —— 期望在哪一侧。 |
| `path` | 是 | 相对路径模板(可以含 `{{inputs.x}}`),相对于该侧的项目根。不允许 `..`。 |
| `required` | 否 | 默认 `true`。`false` 表示缺了只记一笔,不算失败。 |

**核对发生在命令成功之后。** 任务本身报成功后,daemon 会逐个核对声明输出的存在性**和**类型(超时 60 秒)。宿主侧用会识别符号链接的元数据检查加边界检查;远端侧跑一条 SSH 命令,把每个路径报成 `dir`、`file` 或 `missing`。路径存在但类型不对算问题,不算通过。

一旦声明了输出,而某个**必填**输出没有产出,**只有退出码 0 就不再算成功**:该步被标为失败,错误码 `artifactMissing`,退出码强制为 `-1`,消息里点名是哪个输出。核对通过的输出会替换掉 job 记录里"按模板猜出来的"产物路径,所以 job 列的是真实存在的东西。

一个具体的失败例子 —— 一条从没产出 ipa 却声明了它的 `push`:

```json
{ "kind": "push", "outputs": { "ipa": { "type": "file", "side": "remote", "path": ".cgw/out/Runner.ipa" } } }
```

push 本身成功了,但远端没有 `.cgw/out/Runner.ipa`,于是这一步以 `artifactMissing` 失败,消息里点名 `remote:.cgw/out/Runner.ipa`。加上 `"required": false` 结果就变了:这一步成功,只是运行消息末尾多一句"可选输出没产出"的说明。

## 引用另一个步骤的输出

在批量工作流里,后面的步骤可以通过把 `{{steps.<步骤 id>.outputs.<名字>}}` 写进后续步骤的 `variables` 取值,来消费前面步骤**核对过**的路径。产出那一步必须更早、必须带步骤 `id`,并且必须声明过那个输出。

```json
{ "kind": "build", "id": "t-build", "manifest": "flutter-ios",
  "outputs": { "app": { "type": "directory", "side": "remote", "path": ".cgw/out/Runner.app" } } }
```

```json
{ "kind": "pull", "id": "t-pull",
  "inputs": { "appPath": { "type": "path", "side": "remote" } },
  "sources": [{ "side": "remote", "path": "{{inputs.appPath}}" }],
  "to": { "side": "host", "path": "artifacts" } }
```

```json
{ "id": "b-release", "name": "release",
  "steps": [
    { "taskId": "t-build", "id": "package" },
    { "taskId": "t-pull", "variables": { "appPath": "{{steps.package.outputs.app}}" } }
  ] }
```

只有真正通过核对的输出才会被公布,所以消费方永远读不到一个"只是声明过"的路径。如果这一步开始时引用解析不了,该步立刻以错误码 `stepRefMissing` 失败 —— 而不是把字面量 `{{steps...}}` 递给任务、让它在离现场很远的地方炸掉 —— 后续步骤记为跳过。

**侧一致性规则。** 如果消费步骤对应的任务为接收的这个值声明了输入,而该输入声明了 `side`,那么这个侧必须等于产出输出的 `side`。用 `host` 输入去接一个 `remote` 输出,保存阶段就会以 `batchInvalid` 拒绝,因为那个路径在错误的机器上看起来同样合法。把输入的 `side` 留空就不做这项检查。批量的结构、顺序与重试见[批量与工作流](Batches-and-Workflows.zh-cn)。

## 重试与记录下来的输入快照

每个 job 都记下**它用到的输入**,按任务分片,键是 `<任务id>.<输入名>`。

重试会创建一个**新 job**,从失败的那一步开始(也可以用 `--from N` 指定步骤,或 `--from-start` 从头开始);已经成功的步骤不会重跑,步骤编号仍与原批量对齐。

重试从记录下来的快照解析输入,而不是从任务当前的值,所以"改了任务变量再重试一个旧 job"重跑的是**当时记录下来的那一版**,日志与结果因此始终可审计。命令行入口是 `cgw jobs retry <jobId>`。

## 不运行也能看

| 命令 | 给你什么 |
|---|---|
| `cgw tasks <项目>` | 任务列表:id、kind、name,以及该类型真正要紧的字段。 |
| `cgw task <项目> <任务id>` | 单个任务存放的 JSON(模板原文,不是展开结果)。 |
| `cgw tasks preview <项目> <任务id>` | **展开后**的那一份,只渲染不执行。 |
| `cgw schema task` | 任务请求体的 JSON Schema。 |

`cgw tasks preview` 先解析输入与输出,再渲染真正会跑的那一份:`build` 返回 `kind`、`command`、`artifactPath`、`artifacts`、`xcodePath`,文件类任务返回解析后的 `from` / `to` / `target` / `sources` / `outputName` 与各开关。响应里还带解析后的输入与声明的输出,它告诉你这次运行会用哪些值、跑完会核对什么。缺必填变量返回 `variableRequired`,契约有问题返回 `inputInvalid` —— 两者都不碰目标机。

HTTP 端点是 `POST /api/projects/{id}/tasks/{taskId}/preview`。Schema 走 `GET /api/schema/task`,或者 MCP 资源 `cgw://schema/task`:它由 Rust 结构体生成(所以不可能和解析逻辑漂移),是 draft-07,七个 `oneOf` 分支逐条写明每种类型必填哪些字段。

## 常见坑

- **声明了这一步产不出的输出。** 这就是经典的 `artifactMissing`:命令退出码是 0,但声明的产物不在,于是这一步失败。只声明这一步真会产出的东西,或者标成 `required: false`。
- **路径里有 `..`。** 保存阶段会被拒,在 `{{inputs.x}}` 展开**之后**还会再拒一次 —— 输入取值塞不进来。
- **忘了 `overwrite`。** `copy`、`move`、`pull` 在目标已存在时,不写 `overwrite` 就拒绝覆盖,这次运行直接失败。
- **把 `to` 当成目录。** 它就是完整的目标路径,`{"to": {"path": "dst"}}` 会把东西正好放在 `dst` —— 这正是复制能改名的地方。
- **以为 `build` 会自己 push。** 它从不这么做;没推送过的项目会在空的远端目录上失败。
- **CRLF 脚本。** 除非你确实要原样字节,否则让 `keepCrlf` 保持 false,不然 Windows 检出的脚本可能报 `bad interpreter: /bin/sh^M`。
- **引用了一个没有 `id` 的步骤**,或者以为声明过就等于存在:只有核对通过的输出才会公布,输出要被后续步骤消费的那一步必须写 `id`。

← [首页](Home.zh-cn)
