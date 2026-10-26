# 客户机与目标机环境

客户机就是一台 SSH 目标机 —— 通常是 CompileGateway 把源码推上去、在其上构建的 macOS 打包机。

## 什么是客户机

一台客户机就是一台 SSH 目标机:地址、端口、凭据、若干自定义标签,以及这台机器上的一个基准
目录。每个项目只能指定一台客户机,多台客户机可以被多个项目共用。推送、拉取、构建、复制、
剪切、删除、导出用的都是这台机器 —— 除此之外没有第二处"远端"设置。

daemon 把整份列表存在 `clients.json` 里,和 `config.json` 同目录:

```json
{
  "id": "c-4f2a91b0c3d8",
  "name": "mac-mini",
  "host": "192.168.1.20",
  "port": 22,
  "auth": {
    "method": "key",
    "username": "builder",
    "password": "",
    "keyPath": "C:/Users/me/.ssh/id_ed25519",
    "passphrase": ""
  },
  "tags": ["ios", "office"],
  "baseDir": "~/CompileGateway",
  "baseDirResolved": "/Users/builder/CompileGateway",
  "hostKey": "SHA256:9f3c1p...",
  "hostKeyLine": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI...",
  "hostLock": false
}
```

`id` 由 daemon 生成:`c-` 加 12 位十六进制。`hostKeyLine` 可能是空的 —— 只有真正连接过一次、
看到过主机公钥(或从 `known_hosts` 导入过)之后才会有值。

## 字段参考

| 字段 | 含义 |
|---|---|
| `id` | daemon 生成的标识(`c-…`);每条 CLI 命令与 API 路径都用它。 |
| `name` | 显示名。新建时不给 `--name` 就默认用主机地址。 |
| `host` | 目标机的主机名或 IP。必填。 |
| `port` | SSH 端口,缺省 `22`。 |
| `auth.method` | `password` 或 `key`,缺省 `password`。 |
| `auth.username` | SSH 用户名。留空时回退到 daemon 自己的 `USERNAME` / `USER` 环境变量。 |
| `auth.password` | `method` 为 `password` 时使用的密码。 |
| `auth.keyPath` | `method` 为 `key` 时使用的私钥文件。密钥由 **daemon** 读取,所以这个路径在 daemon 那台机器上。 |
| `auth.passphrase` | 私钥口令。`ssh-keygen` 默认就给私钥加密,所以这个字段经常要填。 |
| `tags` | 自定义设备标签;WebUI 搜索框覆盖 名称 / IP / 端口 / 标签。 |
| `baseDir` | **目标机**上的基准目录,缺省 `~/CompileGateway`。 |
| `baseDirResolved` | 上一次验证基准目录时远端 `pwd` 回的绝对路径。由验证写入,不是手填。 |
| `hostKey` | 已记录的主机指纹(`SHA256:…`)。空 = 从来没连过。 |
| `hostKeyLine` | 主机公钥原文(`ssh-ed25519 AAAA…`),留着是为了把信任关系导出成 `known_hosts`。它是 `hostKey` 的缓存,指纹一变就一并丢弃。 |
| `hostLock` | 主机级独占互斥开关。为 `true` 时，派发到该物理客户机的任务严格串行执行（全局 `hostLock` 或单机开启任一即生效）。缺省 `false`。 |

**密钥只写不读。** `password` 与 `passphrase` 存在 daemon 那台机器的 `clients.json` 里,但接口
从不回传;客户机记录只给 `hasPassword` / `hasPassphrase`,够 WebUI 显示"填过没有"。保存时
**不给** `password` / `passphrase` = 保持原值,**给空串** = 清除;所以编辑表单里留空不算改动。
公钥原文不是秘密,但客户机记录里只报 `hasKeyLine`,原文由 `known_hosts` 导出接口给出。

## 验证方式与密钥格式

| `auth.method` | 发什么 | 说明 |
|---|---|---|
| `password` | `username` + `password` | 密码为空时直接报"没有填写密码"。 |
| `key` | `username` + 私钥文件 | 可用 `passphrase` 解锁;签名用 SHA-256。 |

已确认能读进来的格式:

- OpenSSH 原生格式(`BEGIN OPENSSH PRIVATE KEY`,加密与否都行)、PKCS#1
  (`BEGIN RSA PRIVATE KEY`)、PKCS#8(`BEGIN PRIVATE KEY`,以及加密的
  `BEGIN ENCRYPTED PRIVATE KEY`)、SEC1(`BEGIN EC PRIVATE KEY`);
- 传统 PEM 加密且 `DEK-Info` 为 `AES-128-CBC`(也就是 `ssh-keygen -m PEM -N <口令>` 的产物);
- PuTTY `.ppk` v2 / v3 格式（已按 PuTTY 官方规范通过测试套件校验）；
- 行尾是 CRLF 的密钥(经 Windows 编辑器拷来拷去),加密与否都行。

传统 PEM 加密若用的是别的算法(`AES-192-CBC` / `AES-256-CBC` / `DES-EDE3-CBC`)会被**明确拒绝**,
错误里直接给出转换命令(`openssl pkcs8 -topk8`),而不是含混地报"口令不对"。带口令的私钥没填口
令时会提示"这是带口令的私钥,请填密钥口令";口令填错也会明确说是口令不对。

## 测试连接

测试会真正开一次 SSH 会话、执行 `echo CGW_OK`,并确认这行回显真的回来了。连接、握手、认证
合计 20 秒上限;测试本身 15 秒放弃。

```bash
cgw clients test mac-mini
cgw clients test mac-mini --json
```

WebUI:进入该客户机的实例视图,点「测试连接」。CLI 的 `--json` 给出下面这整个对象。

| 字段 | 含义 |
|---|---|
| `ok` | 整次测试是否成功。 |
| `message` | 人话结果,例如 `connected: builder@192.168.1.20:22`,或失败原因。 |
| `output` | 远端命令的 stdout/stderr 合并文本(`CGW_OK` 那次回显),供人过目。 |
| `hostKey` | **本次**连接看到的主机指纹。首次成功时 daemon 会把它和公钥原文一起记到客户机上。 |
| `presentedKey` | 指纹不一致时,对端**这次实际出示**的指纹。 |
| `mismatch` | 为 `true` 表示这次失败是指纹不一致,而不是网络或凭据问题。 |

## 主机指纹信任(TOFU)

第一次连接成功时记下主机钥匙的 SHA-256 指纹(`hostKey`)和公钥原文(`hostKeyLine`);之后每次
连接都必须出示同一个指纹,不一致就直接拒绝连接:

```text
主机公钥指纹与上次记录的不一致,已拒绝连接。上次: … 本次: …
```

当目标机重建虚拟机、重装系统或更换硬件时，主机公钥指纹会发生改变。CompileGateway 默认通过阻断连接提供中间人防护，同时在失败结果中完整透传本次实际出示的指纹（`presentedKey`）及 `mismatch: true`，由用户决定是否接受并信任新指纹：

- **信任新指纹** —— `cgw clients hostkey mac-mini --trust SHA256:…`,把出示的指纹记下来替换旧的;
- **清除记录** —— `cgw clients hostkey mac-mini --forget`,下次连接重新按首次信任处理;
- **只看一眼** —— `cgw clients hostkey mac-mini` 打印当前记录。

形状不是 `SHA256:…` 的值在写盘之前就被拒。信任与清除都会往这台实例的日志里写一条 `WARN`,
因为让掉一次不一致就等于让掉一次中间人防护。

WebUI 上同一个选择出现在测试连接撞到不一致时:确认框把**记录值和对端出示值并排摆出来**。
实例的**状态**页也显示当前记录的指纹,并提供一个清除按钮。

### `known_hosts` 的导入与导出

也可以在第一次连接**之前**就把钥匙定下来,或者把信任关系带走:

```bash
# 从 CLI 能读到的文件导入(文件可以在用户自己的机器上),或者从标准输入来
cgw clients hostkey mac-mini --import ./known_hosts
ssh-keyscan -p 2222 192.168.1.20 | cgw clients hostkey mac-mini --import -

# 导出一行标准 known_hosts,可直接追加进 ~/.ssh/known_hosts
cgw clients hostkey mac-mini --export
```

导入遵循 OpenSSH `known_hosts` 的规矩:逗号分隔的多个主机、`*` / `?` 通配、`!` 取反、
`[主机]:端口` 条目,以及 `|1|salt|hash` 形式的哈希主机名(用 HMAC-SHA1 比对)。主机名不区分
大小写;不带端口的记录只对 22 端口生效,所以客户机用的不是 22 端口时,导入会说明为什么
没匹配上,而不是默默返回空。

导出需要公钥原文;只记了指纹的话,导出给的是一行注释加一条警告 —— 指纹是单向摘要,要拿到
标准格式得再连一次。WebUI 的状态页上也有这两个按钮(选文件的框浏览的是 **daemon** 那台机器)。

### 导入会拒绝什么

| 被拒的情况 | 原因 |
|---|---|
| 命中 `@revoked` 记录 | 那把钥匙被明确吊销,永远不能当主机钥匙。 |
| 命中 `@cert-authority` 记录 | 那是给主机签证书的 CA,不是主机自己的钥匙;本工具不支持主机证书。 |
| 文件里有**多把不同**的钥匙都能对上这台客户机 | 不猜,把候选指纹列出来让人选。 |
| 记录里已有一把**不同**的钥匙,却没有 `--force` | 静默替换正是 TOFU 要防的事。 |
| 不认识的 `@…` 标记、SSH1 时代的 `RSA1` 行、解不出的钥匙 | 报成 `第 N 行:原因` 并跳过,绝不会变成受信任的钥匙。 |

文件里其它看不懂的行只是被记下来;能读的行照样生效。

## 基准目录

每台客户机有一个基准目录:这台机器上放源码与产物的根。缺省是 `~/CompileGateway`。它存在的
理由是 CompileGateway 不在用户家目录里直接干活 —— `clean` 推送会先删掉目标再重建,必须有一个
自己的根。

设置时是**真的连上去验证**:

```bash
cgw basedir mac-mini '~/CompileGateway'
cgw basedir mac-mini '/Users/builder/builds' --no-verify
```

daemon 会连上去跑 `mkdir -p <目录> && cd <目录> && pwd`(20 秒上限):

- **成功** —— 存盘,并把 `pwd` 回来的绝对路径记成 `baseDirResolved`;
- **失败** —— **不落盘**(连不上的目录绝不会被存下来),远端报错原样显示;
- `--no-verify` 跳过连接,按填的内容直接存。

`~` 开头的路径不加引号(加引号 `~` 就不展开了);含空格或特殊字符才用单引号包裹。WebUI 的实例
**状态**页保存的是同一个值,走的也是同一套验证。

项目的远程落点是 `<基准目录>/<slug>`,slug 由项目名转出来(`Spice Compose` → `spice-compose`)。
用缺省基准目录时,名为 `app` 的项目就落在目标机的 `~/CompileGateway/app`。改基准目录会影响
**新**项目目录建在哪;已经存在的远端目录不会被搬走。

## 环境检测

实例的**环境**页签回答"这台机器到底能不能构建这个工程"。一次检测要跑 11 个探针外加一次 Xcode
清点,按平台分组:

| 分组 | 探针 |
|---|---|
| `xcode` | Xcode(`xcodebuild -version`)、CocoaPods(`pod --version`)、Homebrew(`brew --version`) |
| `gradle` | JDK(`java -version`)、Gradle(`gradle --version`) |
| `flutter` | Flutter(`flutter --version`) |
| `dart` | Dart(`dart --version`) |
| `rust` | Cargo(`cargo --version`)、rustc(`rustc --version`) |
| `common` | rsync(`rsync --version`)、XcodeBuildMCP(`xcodebuildmcp --help`) |

除这些探针,一次检测还会拿到:

- `uname -s`:判断平台,也决定 Xcode 那一段要不要跑;
- 磁盘情况,来自 `df -Pk "$HOME"`(总量 / 已用 / 可用 / 挂载点 / 使用率);
- Xcode 清点 —— 只在 `Darwin` 上,脚本遍历 `/Applications/Xcode*.app`,对**每个实例**用它自己的
  `DEVELOPER_DIR` 跑一次 `xcodebuild -version`,所以每个安装报的是自己的版本号和 Build;
- Xcode 许可状态,来自 `xcodebuild -license status`。

以上全部拼成**一个** POSIX shell 脚本、在**一次** SSH 会话里跑完,每段前面打一个 `###cgw:<id>`
标记,回来在本地按标记切分。每个探针都有 `command -v` 守卫,工具不在就吐 `__CGW_MISSING__`,
所以缺一个工具不会让整次检测失败。脚本整体的上限是 **90 秒**。

结果按 `platform` 分组;一个全都没命中的组也会照样列出并标成不可用,好让你看出缺什么。报告里还
带着 `os`、`elapsedMs`、`checkedAt`、每个探针的原始输出,以及类似 `8 / 11 个工具就绪` 的消息。

### 缓存与重跑

读缓存和真跑是两件事:

```bash
cgw env mac-mini            # 真跑一次(最多 90 秒)
cgw env mac-mini --cached   # 只读缓存
cgw env --script            # 只打印要执行的脚本,不连任何机器
```

WebUI:实例的**环境**页签读的是缓存,点检测按钮才真跑。缓存按客户机放在 daemon 的**内存**里,
不写盘,所以 daemon 重启后就没了。任务编辑框里的 Xcode 选择器读的也是这份缓存,不会为了列个
选项再连一次。

## 按次选择 Xcode

CompileGateway **不会**去改目标机的全局 `xcode-select`。build 任务上有一个 `xcodePath` 字段填
`.app` 路径,渲染命令时它变成一句 `export DEVELOPER_DIR=…`,放在 `cd` 进远端目录之后、所有构建
步骤之前,所以每一步都吃得到:

```text
cd ~/CompileGateway/app && export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer && ./gradlew … && xcodebuild …
```

路径按"一个值"做 shell 转义,`/Volumes/My Disk/Xcode.app` 这种带空格的照样安全。`xcodePath`
留空 = 用目标机全局选中的那个。任务编辑框从环境缓存里列出可选实例,所以先跑一次环境检测,
这个选择器才有内容。

## 故障排查

| 现象 | 多半是 | 怎么办 |
|---|---|---|
| `无法连接 <host>:<port> - …` / 连接被拒 | 那个端口上没有 sshd、地址写错,或者被防火墙挡了。 | 检查 `host` / `port`;在 daemon 那台机器上试 `ssh -p <端口> <用户>@<主机>`。 |
| `连接或认证超时(20s),对端可能没有完成 SSH 握手` | 有东西接受了 TCP,却从不完成 SSH 握手。 | 确认指向的是 SSH 服务,而不是 22 端口上的别的服务。 |
| `认证失败:服务器拒绝了用户 … 的凭据` | 用户名或密码不对,或者服务端不允许这个账号用公钥认证。 | 检查 `auth.username` / `auth.password`;确认目标机的 `sshd_config` 允许该方式。 |
| `主机公钥指纹与上次记录的不一致 …` | 目标机换了主机钥匙(重建虚拟机、重装、换机器)—— 也可能是真的劫持。 | 对比两个指纹,然后信任新的(`cgw clients hostkey <id> --trust SHA256:…`)或清除记录(`--forget`)。 |
| `读取密钥失败:… 看起来是带口令的私钥,请在「密钥口令」里填上它` | `ssh-keygen` 给私钥加了密,而 `auth.passphrase` 是空的。 | 填 `auth.passphrase`(`cgw clients edit <id> --key-passphrase …`)。 |
| `读取密钥失败:密钥口令不对,或者这个文件不是用该口令加密的` | 口令解不开这个文件。 | 核对口令;若是传统 PEM 且用 AES-256/192 或 DES-EDE3,用 `openssl pkcs8 -topk8` 转换。 |
| `读取密钥失败:<路径> - <错误>` / `没有选择密钥文件` | daemon 读不到这个路径:空的、写错了、属于别的用户,或者被 ACL 挡住。这里是纯 Rust 的 SSH 客户端,**不做** OpenSSH 那套"私钥权限过于开放"检查。 | 换成 daemon 进程读得到的路径;文件必须在 daemon 那台机器上,不在目标机上。 |
| 保存基准目录失败,什么都没存下 | 验证用的 `mkdir -p && cd && pwd` 失败了(没权限、路径写错、只读卷)。 | 看远端报错,修路径或权限再存;`--no-verify` 会不加验证直接存。 |

## 接下来看哪里

- [命令行](CLI.zh-cn) —— 客户机的每条命令、退出码与 `--json` 输出。
- [网页界面](WebUI.zh-cn) —— 客户机列表、实例视图、环境页签与两个主机指纹按钮。
- [故障排查](Troubleshooting.zh-cn) —— 错误码与跨模块的常见症状。

← [首页](Home.zh-cn)
