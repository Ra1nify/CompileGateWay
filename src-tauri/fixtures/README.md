# 测试夹具

## `probe-linux.txt`

`env::probe_script()` 在一台真实 Linux 机器(WSL Ubuntu)上跑出来的**原始 stdout**,
逐字节存下来当回归夹具。

怎么重新生成:

```bash
cd src-tauri
cargo run --bin compilegateway-cli -- env --script > /tmp/probe.sh
bash /tmp/probe.sh > fixtures/probe-linux.txt
```

它锁住的是:

- 脚本在真 POSIX shell 下的行为(`command -v` 守卫、分段标记、`df -Pk` 输出格式);
- 解析器对这些真实形状的处理,包括几个容易写错的点:
  - 缺失的工具必须报缺失,不能凭空造版本号;
  - `rsync --version` 会吐多行(第二行起是版权信息),版本只在第一行;
  - 机器上装了但跑不起来的工具(夹具里 flutter/dart 因为脚本是 CRLF 而报错)要
    "算存在但读不出版本",不能算命中版本,也不能算失败;
  - 非 Darwin 平台的 Xcode 段必须是空的,不能造出实例或许可结论。

`src/env.rs` 的 `parses_a_captured_real_linux_run` 用例就断言这些。

## 私钥夹具(一次性生成的测试密钥,没有任何真实凭据)

口令一律是公开的 `cgw-test-pass`。它们锁住的是「**哪种私钥格式读得进来**」这件事 ——
现实里私钥有好几种历史形态,只测过其中一种就等于没测。

| 文件 | 头 | 格式 | 状态 |
|---|---|---|---|
| `plain-ed25519.key` | `BEGIN OPENSSH PRIVATE KEY` | OpenSSH(不加密) | 支持 |
| `encrypted-ed25519.key` | 同上 | OpenSSH + 口令 | 支持 |
| `rsa-pkcs1.key` | `BEGIN RSA PRIVATE KEY` | PKCS#1(RSA) | 支持 |
| `rsa-pkcs1-enc.key` | 同上 + `Proc-Type: 4,ENCRYPTED` | PKCS#1 + 传统 PEM 加密(**AES-128**-CBC) | 支持 |
| `rsa-pkcs8.key` | `BEGIN PRIVATE KEY` | PKCS#8 | 支持 |
| `rsa-pkcs8-enc.key` | `BEGIN ENCRYPTED PRIVATE KEY` | PKCS#8 + PBES2(`openssl genpkey -aes-256-cbc`) | 支持 |
| `ec-pem.key` | `BEGIN EC PRIVATE KEY` | SEC1(EC) | 支持 |
| `ec-pem-enc.key` | 同上 + `Proc-Type` | SEC1 + 传统 PEM 加密(**AES-256**-CBC) | **不支持**,但会给出转换命令 |

`ssh-keygen` 生成:

```bash
# 注意别把输出管道给 `Select-Object -First N` 之类的命令 —— 那会在写完之前把
# ssh-keygen 掐掉,留下一份**截断的密钥**,症状是"口令明明对却解不开",很难查。
ssh-keygen -q -t ed25519 -N cgw-test-pass -f encrypted-ed25519.key
```

`openssl` 才能生成的那两个(PKCS#8 加密 / 传统 PEM + AES-256)用 `make-openssl-keys.sh`
生成,需要 WSL 或 Linux 上的 openssl:

```bash
cd src-tauri/fixtures && ./make-openssl-keys.sh
```

脚本里有两条踩过的坑:

- **别用 `ssh-keygen -y -f <私钥>` 导公钥**。这个仓库在 `/mnt/f`(drvfs)上,权限一律显示
  0777,ssh-keygen 会以 `UNPROTECTED PRIVATE KEY FILE` 拒绝读私钥。所以走
  "openssl 导 SPKI → `ssh-keygen -i -m PKCS8` 转 OpenSSH"这条路(公钥不看权限)。
- 公钥统一存成 **OpenSSH 格式**,用例才能直接 `PublicKey::from_openssh` 拿它验签。

`src/ssh.rs` 的 `key_tests` 用它们验三件事:每个格式都读得进来、**读出来的能真的签名**
(而且配对公钥验得过、改一个字节就验不过)、错误提示是人话 ——
"口令对能解开 / 不给口令时提示去填口令 / 口令写错被识别成口令问题"这三条也在里面
(ssh-key 对后两种都只回一句 `cryptographic error`,所以 `ssh::load_private_key` 专门做了翻译)。

## `e2e-cat.ps1`

受控读文件(`cgw cat` / `GET /api/projects/{id}/file`,设计文档 §7.4)的端到端夹具。
**全离线**,走宿主侧。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-cat.ps1
```

15 项断言,分三组:**正常读**(文本、子目录、`cmd` 真重定向到本地文件后内容逐字节一致)、
**封顶与二进制**(400 KB 文本只读 256 KB 且报告真实大小;PNG 判为二进制且 `text` 为空)、
**边界**(目录被拒、不存在报错、`..` 逃不出去、符号链接逃逸被拒 ——
最后一条在能建符号链接的机器上才会跑,建不出来就跳过)。

两个脚本坑,都栽过:`PowerShell` 的 `Cat` 是 `Get-Content` 的别名(和 `$host`/`$home` 一样,
自定义函数别叫这些名字);`| Set-Content` 会把原生命令输出按行拆了重排,所以"重定向到文件"
要用 `cmd /c "... > 文件"` 才是真的字节级重定向。

## `e2e-export.ps1`

导出压缩包 + 下载的端到端夹具(设计文档 §9 的 Export)。**全离线**:走宿主侧导出,
不需要 SSH,但验的是**字节**——

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-export.ps1
```

17 项断言:导出(默认名 `{项目名}-{unix 秒}.zip` 和指定名)、列出、下载、
**解开压缩包比对**(zip 魔数、条目结构、解开后文件的 SHA-256、中文文件名与内容)、
`--save` 给目录 / 给文件名两种写法,以及两条边界 —— 路径穿越拿不到 `exports/` 之外的文件、
不存在的名字返回 404;没给路径又没有构建产物时明确拒绝(而不是默默打包整个项目)。

它顺带盯住一个容易漏的点:压缩包里的**顶层是来源的名字**(`src/`),因为导出会先把来源
暂存到同名目录再打包;目录本身也会作为条目写进去(这样空目录不会丢)。

## `e2e-counts.ps1`

端到端夹具:起一个临时 daemon,把 `copy` / `move` / `delete` / `export` / `pull`
各跑一遍(外加一个混合 batch),然后断言 `cgw job <id> --json` 里的 `files` / `bytes`
跟实际搬运的内容对得上。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-counts.ps1
```

**全离线,而且不碰任何真实机器**,这一点是它的主要价值:

- 所有任务都只走**宿主侧**(`side: host`),而 host → host 的文件操作不需要 SSH。
  所以 client 是指向 `127.0.0.1:1` 的假客户机(永远连不上)也照样跑;
- 脚本把 daemon 的 `USERPROFILE` 指到 `%TEMP%\cgw-cnt\home`,
  于是 `projects::host_root()` 落在沙箱里,用户自己的 `~/CompileGateway` 不会被写;
- daemon 的 `config.json` 用 `"port": 0`,端口从 `runtime.json` 里读,不会撞上用户正在跑的实例。

它锁住的是**"算出来的数"和"记下来的数"一致** ——
`fileops::OpOutcome` 里的计数只是第一步,`server.rs` 里 `execute_task` 的每个分支、
batch 的累加、以及 `jobs::finish` 的落盘都得跟上。这几个地方漏一个,
`cgw job` / WebUI 就会显示 0,而日志消息里却是对的(export 那次就是这么被抓出来的)。

### 密钥夹具与旧文档

上面那张私钥表还替掉了一段旧说明(原本只说了"带口令的 ed25519"和"口令不对时要说人话")。
原来那两份密钥文件仍然在,只是现在和 PKCS#1 / PKCS#8 / SEC1 / 加密 PEM 摆在一起看。

## `e2e-schema.ps1`

JSON Schema(设计文档 §2)的端到端夹具:起一个沙箱 daemon,建一个项目,
把**七种任务类型各存一个**,然后
(a) 验 CLI 面(`cgw schema` / `cgw manifests schema`),
(b) 验保存期校验(空 batch / 空 taskId / 引用不存在的任务必须被拒),
(c) 把 `mcp/test/schema.mjs` 拉起来,让 ajv 拿这些**真实文档**去验 schema。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-schema.ps1
```

同样全离线(client 指向 `127.0.0.1:1`,因为只做"存"不做"跑"),
同样把 `USERPROFILE` 指到沙箱。

两个容易踩的点,夹具里都注释了:

- **新建任务时 id 是服务端生成的**。请求体里给的 `id` 只用来"选中一条已有任务去覆盖",
  服务端不会照抄。所以夹具存完要按 `kind` 把真实 id 读回来,batch 才有东西可引用。
- **`cgw tasks` / `cgw batches` 的 `--json` 吐的是数组**,不是 `{tasks:[…]}` /
  `{batches:[…]}`(MCP 那些工具才是后者)。这个坑踩过三次了,写夹具时先 `--json` 看一眼形状。
- 列表里批量/任务的字段名是 camelCase(`taskId`、`variables`),和项目 JSON 一致。

这个夹具第一版还踩了 `$host` —— 它是 PowerShell 的只读变量(`$home` / `$pid` 同理),
赋值会直接抛错。这类名字在测试脚本里最好避开。

## `e2e-known-hosts.ps1`

`known_hosts` 导入/导出的端到端夹具。**全离线**:只验"解析 → 匹配 → 存 → 取 → 拒绝"这套逻辑,
一次 SSH 都不连(客户机的 `host` 用一个不存在的域名,端口故意不是 22,顺便验 `[host]:port` 的规矩)。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-known-hosts.ps1
```

22 项断言,覆盖:正常导入(含哈希 hostname 与明文两行指向同一把钥匙)、重复导入、
导出标准行、导出→再导入往返、从 stdin 读(`-`),以及**必须拒绝**的五种情况 ——
`@revoked`、`@cert-authority`、同一 host 两把钥匙、端口对不上、以及不带 `--force` 就想替换已有记录。

公钥用的是 `src/known_hosts.rs` 用例里那几把**测试公钥**(没有对应私钥),
哈希 hostname 的 salt/hash 是用 .NET 的 `HMACSHA1` 独立算出来再写进夹具的 ——
不拿被测代码自己算,否则就是自己验自己。

## `e2e-mcp.ps1`

MCP 适配层的端到端夹具(设计文档 §3、§12 Phase 6)。**全离线**,不需要装 Node、
也不需要任何 MCP 客户端 —— 它直接按 Streamable HTTP 的规范对 daemon 的**第二个端口**
发 JSON-RPC。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-mcp.ps1
```

50 多项断言,分六段:

1. **启动即提供**:`runtime.json` 里同时有 daemon 的地址和 `mcpUrl` / `mcpPort` / `mcpEnabled`,
   而且两个端口号不一样(老配置里没有 `mcp` 段时按默认值走,不用手改);
   `/healthz` 报告 `daemonPort` 和 43 个工具;
2. **协议面**:`initialize`(协议版本按客户端报的来)、通知回 202 且**没有响应体**、
   `tools/list` 43 个且**一个悬空 `$ref` 都没有**(嵌入 daemon 的 schema 时已就地展开)、
   `resources/list` 8 份、`resources/read` 拿到 draft-07;边界:错误折成 `isError` 且带
   `structuredContent.error.code`、未知工具、未声明的方法回 `-32601`、坏 JSON 回 400、
   `GET /mcp` 回 405、未知路径 404;
3. **工具真的打到 daemon 上**:经 MCP 存客户机 → 登记项目 → 建 copy 任务 → 执行,
   轮询到 `state=ok`,并断言这个 job 上标着 **`source: "mcp"`**(设计文档 §10:
   一眼看出哪些活是 AI 干的);
4. **教学三层**(agent 该知道的东西,不能只写在 README 里):`instructions` 里必须
   **同时**点名 `cgw://guide` 和 `cgw://cli`(双重保险),能力里声明了 prompts;
   `cgw://guide` 是一页真 playbook(有"Getting a project running for the first time"/
   "What to do when it fails"两节,
   mimeType 是 markdown);`cgw://cli` 带着**完整命令面**(≥60 条,含 `manifests.refresh`)、
   8 个退出码、一段"什么时候该用 CLI 而不是 MCP";`prompts/list` ≥3 个模板,
   `prompts/get` 取得到正文,必填参数没给会明确说是哪个;
5. **宿主目录白名单**(设计文档 §6.1):白名单之外的目录经 MCP 登记项目**也会被拒**,
   错误里让你去加 `allowedRoots`;加完再登记就成功 —— 和 browse / 探测是同一套判断;
6. **CLI / WebUI 都能配它**:`cgw mcp` / `cgw mcp url` / `cgw config show` 的显示、
   **改端口当场生效**(写死的端口原样生效、旧端口立刻关闭、config.json 落盘)、
   **端口被占用时明确报错而且不把已经能用的 MCP 弄没**(回滚到原配置)、关掉再打开。

MCP 的其他验证(`mcp/test/*.mjs`)需要 Node,而且第三个用例要一个活着的 daemon;
那些是协议级测试(stdio 转发桥 / 并发锁 / HTTP 端点 / ajv 校验),见 README 的
「MCP 适配层 → 怎么验证的」。

## `e2e-manifests.ps1`

构建类型目录(`manifest/`)的端到端夹具(设计文档 §6.2 / §6.4)。**全离线**。

```powershell
cd src-tauri; pwsh -NoProfile -File fixtures\e2e-manifests.ps1
```

这一轮把"内置预设"这个概念拆掉了:构建类型的 json 就是 **exe 同目录的 `manifest/`**
里几个普通文件。夹具验的就是这套语义,20 多项断言:

1. **首次启动**:目录不存在 → 建出来 + 把自带的 4 份模板复制进去;
   `config.json` 里不需要写任何东西(缺省即开);
2. **第二次启动**:目录已经存在 → 一个字节都不动 —— 你改过的没被覆盖、你删掉的没被补回来;
3. **刷新事件**(daemon 跑着的时候改文件):`POST /api/manifests/reload` 与
   `cgw manifests refresh` 都要报出**新增 / 内容变了 / 已删除**,顺手带回当前清单和
   "有几份不合法",并在日志里留一条;
4. **目录整个删掉**再刷新 → 重新生成 + 重新铺模板;
5. **补齐模板**:`/api/manifests/templates` 只列出缺的,`reset` 只写回缺的,
   你改过的那份一律不碰;
6. **老目录 `variety/`** 会被**原样改名**成 `manifest/`(内容一点不动,包括你自己加的)。

顺带说清楚一条容易误会的语义:刷新报的差异是**相对本次运行中上一次看到的目录状态**,
不是"相对上次启动" —— daemon 起来时会先记一次基线,所以第一次 refresh 不会把
4 份模板全报成"新增"。

