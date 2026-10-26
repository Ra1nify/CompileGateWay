# 快速开始

CompileGateway 把本地源码树推送到 SSH 目标机,在那边完成构建,再把产物拉回来或打包导出。

## 前置条件

| 需要什么 | 说明 |
|---|---|
| Rust **nightly** 工具链 | 用 nightly 构建;装法:`rustup toolchain install nightly`。 |
| Node 18+ | 只在构建 WebUI 资源时需要。 |
| WebView2 运行时(Windows) | Tauri 外壳窗口依赖它。 |
| 一台能连上的 SSH 目标机 | 通常是 macOS 构建机,要能从跑 daemon 的这台机器连过去,所用账号要能在基准目录里写文件。 |

headless daemon 和命令行都不需要图形库,所以一台没装 GTK/WebKit 的常年开机小主机也能构建并运行它们。

## 从源码构建

### 1. 先构建 WebUI

```bash
cd webui-src
npm ci
npm run build
cd ..
```

产物是 `webui/dist`。它必须在构建 Rust 之前就存在:daemon 用 `rust-embed` 在编译期把 WebUI 嵌进自己的二进制。这个目录是有意提交进仓库的,所以直接拉下来的代码本来就带着一份可用的构建结果 —— 只有改过前端源码才需要重新构建。

### 2. 构建三个二进制

```bash
cd src-tauri
cargo build --release
```

产物在 `src-tauri/target/release/`:

| 二进制 | 用途 | 需要图形库 |
|---|---|---|
| `compilegateway` | Tauri 外壳:命令行风格的窗口 + 内嵌 WebUI | 需要 |
| `compilegateway-daemon` | headless HTTP 服务 + 内建 MCP | 不需要 |
| `compilegateway-cli` | 命令行客户端(`cgw`) | 不需要 |

三个目标共用同一份核心逻辑,行为一致。Tauri 相关依赖全部挂在默认的 `gui` feature 后面,所以不打开这个 feature 就不会构建 `compilegateway`。

### 3. 只构建 headless 部分

在没有 GTK/WebKit 开发包的机器上,关掉 GUI feature,只构建那两个不需要图形库的前端:

```bash
cd src-tauri
cargo build --release --no-default-features --bin compilegateway-daemon --bin compilegateway-cli
```

带上 `--no-default-features` 时,Tauri、wry、webkit 既不编译也不链接。

## 跑起来

三个前端都连同一个 daemon:启动任意一个 daemon,再用你喜欢的前端。

```bash
# 1) Tauri 外壳:窗口 + 内嵌 WebUI(需要图形环境)
./target/release/compilegateway

# 2) headless daemon:只有 HTTP,没有窗口也没有 webview(--open 顺带打开 WebUI)
./target/release/compilegateway-daemon

# 3) 命令行客户端
./target/release/compilegateway-cli --help
```

### 第一次启动会生成什么

两种 daemon 都把状态放在可执行文件旁边(读不到就退回当前工作目录),同时往 `CGW_HOME` 里写一份指针文件,这样放在别处的 `cgw` 也能找到它:

| 路径 | 什么时候出现 | 是什么 |
|---|---|---|
| `config.json` | 首次启动 | daemon 配置。所有字段都可以省略,缺的会补默认值。 |
| `runtime.json` | 每次启动 | 真正绑上的端口(`url`、`port`、`listen`、`mcpUrl`、`mcpPort`、`mcpEnabled`、`pid`、`configPath`、`startedAt`)。 |
| `manifest/` | 首次启动 | 构建类型。只在这一步从自带模板铺一次,之后就是你的文件。 |
| `workflows/` | 首次启动 | 跨项目工作流模板,同样的铺一次规则。 |
| `logs/` | 每次启动 | 按天滚动的日志文件,`YYYY-MM-DD.log`。 |
| `projects.json` / `jobs.json` | 第一个项目 / 第一个任务 | 项目登记表与任务(job)历史(最近 50 条)。 |

### `port` 是 `0` 时怎么找端口

`port: 0`(或干脆不写 `port`)表示"让系统挑一个空闲端口"。这时候 `config.json` 回答不了,要看 `runtime.json`(`cat runtime.json`):

```json
{
  "url": "http://127.0.0.1:65251",
  "port": 65251,
  "listen": "lan",
  "mcpUrl": "http://127.0.0.1:3994",
  "mcpPort": 3994,
  "mcpEnabled": true
}
```

`url` 就是 WebUI 与接口的基地址,`mcpUrl` 是 MCP 端点。写死的端口如果被占用是**报错**,不会换一个端口重试。

### 4. 让命令行可用

如果 `cgw` 不在 `PATH` 里,或者 daemon 跑在另一台机器上,用下面任意一种办法:

1. 把 `cgw` 放进 `PATH`,这样在任何目录都能敲:Windows 上 `cgw setup --apply` 会把可执行文件所在目录写进当前用户的 `Path`(不需要管理员),Linux / macOS 上 `cgw setup` 只打印出要加进 shell 配置的 `export PATH=...` 那一行。
2. 设置 `CGW_HOME`,换掉那个每用户共享目录(默认 `~/.compilegateway`)。daemon 把 `runtime.json` 指针写在那里,所以命令行从任何工作目录都能找到它。
3. 设置 `CGW_URL` 直接指定 daemon 地址,例如 `CGW_URL=http://127.0.0.1:65251`,跳过查找过程;`--url <url>` 对单条命令起同样作用。

`cgw setup` 会把上面这些一次性列出来,`cgw doctor` 则回答"现在能不能干活"(有检查项没过时退出码非零)。

## 第一个项目:一条完整链路

### 1. 新建客户机

客户机就是那台 SSH 目标机。认证方式可以是密码,也可以是私钥;加密私钥用 `--key-passphrase`,`--port` 默认 22,`--base-dir` 默认 `~/CompileGateway`。密码和口令是**只写**的:接口永远不会把它们返回给你。

```bash
cgw clients add --host 192.168.1.20 --user builder --password 'secret' --name mac
cgw clients add --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519 --name mac
```

### 2. 测试连接并确认主机指纹

第一次连接会记下主机指纹(TOFU,首次信任)。之后如果对面给出的钥匙变了,这次测试会失败,并把新旧指纹一起摆出来等你决定。`@revoked` 标记、cert-authority 行、一行多把钥匙、静默替换,这几种都会被拒绝。

```bash
cgw clients test mac
cgw clients hostkey mac                       # 看现在记的是什么
cgw clients hostkey mac --trust SHA256:...    # 接受新的指纹
cgw clients hostkey mac --forget              # 清掉记录,下次连接又按首次信任处理
cgw clients hostkey mac --import known_hosts  # 从 known_hosts 文件离线预信任
cgw clients hostkey mac --export              # 打印一行 known_hosts
```

### 3. 设置客户机基准目录

```bash
cgw basedir mac ~/CompileGateway
```

默认会真连上去执行 `mkdir -p` 并把你给的路径解析成绝对路径存下来;加 `--no-verify` 则只保存、不连接。

### 4. 从本地目录登记项目

这里的路径是**跑 daemon 的那台机器**(宿主)上的目录,不是目标机上的。

```bash
cgw projects add --client mac --path /home/me/src/app --name app
```

登记完成后项目有个类似 `p-xxxx` 的 id,远端源码目录由客户机的基准目录推出。`cgw projects` 列出已登记的项目,`cgw projects rm <id>` 只移除登记、不删任何文件。

### 5. 推送源码

```bash
cgw push app --wait
```

源码以 tar 流的形式走 SSH 传输,保留符号链接与权限。带 shebang 的文件默认做 CRLF→LF 归一化(加 `--keep-crlf` 可以原样推送),这一步就是避免 macOS 上出现 `bad interpreter: /bin/sh^M` 的原因;`--clean` 会先清空远端项目目录。

### 6. 建一个构建任务,看构建类型

`build` 任务通过 id 引用一份构建类型,并带上这次运行要用的变量值;`cgw tasks preview` 只渲染命令与展开后的路径,不执行任何东西。

```bash
cgw manifests                       # 列出构建类型;`cgw manifests show xcode` 打印原文
cgw tasks save app --body '{"name":"build","kind":"build","manifest":"xcode","variables":{"scheme":"Runner"}}'
cgw tasks app                       # 列出项目的任务,含生成的 t-xxxx id
cgw tasks preview app t-1a2b        # 只渲染命令与展开后的路径,不执行
```

一份构建类型放在 `manifest/<id>.json`,**文件名就是类型 id**。它声明 `displayName`、`platform`、识别规则(`include.paths`,这些路径必须全部存在)、`variables`(`name` / `label` / `required` / `default`)、`build.steps`(裸命令字符串,或 `{ "id": ..., "run": ... }`)、`build.artifactPath`、`build.artifacts` 和 `cache.paths`。每个 `{{名字}}` 占位符都必须是已声明的变量;保留变量 `signingExtraArgs` 由 `signingMode` 注入,不要自己声明。详见 [构建类型](Manifests.zh-cn)。

### 7. 跑构建

```bash
cgw run app t-1a2b --wait           # `cgw job <jobId>` 看每一步;`cgw jobs logs <jobId>` 看日志
```

`build` **不会先推送**,所以一个没推过源码的项目撞上空的远端目录会失败 —— 先 `push`,或者用一条批量工作流。构建类型里标了 `required` 的变量没给值,会以 `variableRequired` 失败。

### 8. 把产物带回来

```bash
cgw pull app --wait                 # 远端 -> 宿主托管目录
cgw export app --source remote --path .cgw/derived-data/Build/Products --name app.zip
```

`pull` 把声明过的产物复制进这个项目的宿主托管目录(daemon 机器上的 `~/CompileGateway/<项目>/`);`export` 则改成打成一个 zip 放进同一个目录下的 `exports/`(只需要声明 `sources`,外加可选的 `outputName`,不用给目的路径)。列出现有的压缩包用 `cgw exports app`。

## 自检

| 检查项 | 命令 / 位置 | 什么算通过 |
|---|---|---|
| 环境自检 | `cgw doctor` | 它报告 daemon 版本与地址、登录状态、项目数量、构建类型是否合法、日志保留天数。任何一项没过都标成 `FAIL`,`WARN` 另外列出。 |
| daemon 状态 | `cgw status` | 显示端口、监听范围、语言、配置文件和 WebUI 地址,以及客户机与正在跑的 job 数量。 |
| HTTP 存活 | `GET /healthz` | 返回纯文本 `ok`,不需要登录。 |
| WebUI | 浏览器打开 `runtime.json` 里的 `url` | 页面能加载,状态页正常渲染。 |

接下来看哪里:[核心概念](Concepts.zh-cn)讲那套词,[任务](Tasks.zh-cn)讲七种任务类型,[批量工作流](Batches-and-Workflows.zh-cn)讲怎么把任务串起来,[构建类型](Manifests.zh-cn)讲 manifest 的完整字段,[客户机与环境](Clients-and-Environment.zh-cn)讲 SSH 目标机与主机指纹,[WebUI](WebUI.zh-cn) / [命令行](CLI.zh-cn) / [MCP](MCP.zh-cn) / [HTTP 接口](HTTP-API.zh-cn)是四个前端,[配置与日志](Configuration-and-Logs.zh-cn)列出每个配置项和日志读法,出问题看[常见问题](Troubleshooting.zh-cn)。

## 磁盘上会出现的文件

| 文件 / 目录 | 位置 | 内容 |
|---|---|---|
| `config.json` | 可执行文件旁边(或当前工作目录) | daemon 配置;首次启动按默认值生成。 |
| `runtime.json` | 与 `config.json` 同目录,并在 `CGW_HOME` 里再放一份 | 真正绑上的端口与进程信息。 |
| `manifest/` `workflows/` | 与 `config.json` 同目录 | 构建类型与工作流模板,`<id>.json`。两份都归你改。 |
| `logs/` | 与 `config.json` 同目录 | 按天滚动的日志,`YYYY-MM-DD.log`。 |
| `projects.json` `jobs.json` | 与 `config.json` 同目录 | 项目登记表(含各项目的任务与批量工作流);任务历史(最近 50 条)。 |
| `exports/` | 项目的宿主托管目录 `~/CompileGateway/<项目>/` | `export` 产生的 zip 压缩包。 |
| `~/.compilegateway/` | `CGW_HOME`(可改) | 共享指针目录,命令行靠它找到 daemon。 |

---

← [首页](Home.zh-cn)
