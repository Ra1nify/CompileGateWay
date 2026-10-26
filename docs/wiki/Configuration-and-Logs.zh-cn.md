# 配置与日志

CompileGateway 的运行时配置收敛于单一 JSON 文件（`config.json`），并采用按天滚动的结构化日志系统进行运行审计。

## 配置文件 (`config.json`)

配置文件位于可执行文件同级目录（未找到时回退至当前工作目录）。首次启动若不存在，Daemon 会自动生成一份包含默认值的 `config.json`。

```json
{
  "port": 0,
  "language": "auto",
  "listen": "lan",
  "auth": {
    "username": "",
    "passwordHash": "",
    "loginIntervalSeconds": 60
  },
  "logRetainDays": 7,
  "allowedRoots": [
    "F:/Projects"
  ],
  "hostLock": false,
  "mcp": {
    "enabled": true,
    "listen": "lan",
    "port": 3994
  }
}
```

### 配置项字段表

| 字段 | 类型 | 默认值 | 行为与说明 |
|---|---|---|---|
| `port` | `number` | `0` | 主服务端口。`0` 表示由系统分配空闲端口；写死固定端口时若被占用直接报错。**需重启生效**。 |
| `listen` | `string` | `"lan"` | 主服务监听范围：`"lan"`（仅监听 127.0.0.1 回环）或 `"wan"`（监听 0.0.0.0 全网）。**需重启生效**。 |
| `language` | `string` | `"auto"` | 界面与日志语言：`"auto"`（跟随系统区域）、`"zh-Hans"` 或 `"en-US"`。**即时生效**。 |
| `auth.username` | `string` | `""` | 非本机访问时的登录用户名。留空表示不启用认证。 |
| `auth.passwordHash`| `string` | `""` | Argon2id 密码哈希字符串。系统绝不存储明文密码。 |
| `auth.loginIntervalSeconds` | `number` | `60` | 同一来源 IP 连续登录失败的防爆破限流最小间隔（秒，0 为不限流）。**即时生效**。 |
| `logRetainDays` | `number` | `7` | 归档日志自动保留天数（1–3650），过期自动清理。**即时生效**。 |
| `allowedRoots` | `string[]` | `[]` | 宿主机受控根目录白名单。目录浏览、项目登记与类型探测均受此约束。**即时生效**。 |
| `hostLock` | `boolean` | `false` | 全局主机互斥开关。开启后同一台客户机同一时刻只允许执行一个 Job。**即时生效**。 |
| `mcp.enabled` | `boolean` | `true` | 是否启用内置 MCP 服务。**即时生效**。 |
| `mcp.listen` | `string` | `"lan"` | MCP 端口的监听范围（`"lan"` / `"wan"`），与主服务独立。**即时生效**。 |
| `mcp.port` | `number` | `3994` | MCP 独立端口（与主端口无推导关系）。**即时热重载生效**。 |

## 配置修改的三种途径

### 1. WebUI 配置页
在侧边栏进入「配置」页，提交后自动调用 `PATCH /api/config` 写入。

### 2. 命令行 (`cgw set` / `cgw config`)

分类设置子命令：

```bash
cgw set port 65251                      # 修改主端口（需重启 Daemon）
cgw set listen wan                      # 修改监听范围（需重启 Daemon）
cgw set user alice                      # 交互式设定或更新管理员密码
cgw set language zh-Hans                # 切换语言（即时生效）
cgw set login-interval 30               # 修改登录限流间隔（即时生效）
cgw set log-retain 14                   # 修改日志保留天数（即时生效）
cgw set roots add /srv/builds           # 添加宿主机受控根目录（即时生效）
cgw set host-lock on                    # 开启主机互斥（即时生效）
cgw set mcp port 3995                   # 修改 MCP 端口（即时热重载）
cgw set mcp listen wan                  # 切换 MCP 监听范围（即时热重载）
```

或直接提交 JSON Patch：

```bash
cgw config set --body '{"hostLock": true, "logRetainDays": 14}'
```

### 3. MCP 工具 (`cgw_set_config`)
AI Agent 通过调用 `cgw_set_config` 工具直接更新配置。

### 4. 直接修改 JSON 文件
可在 Daemon 停止状态下直接编辑 `config.json`。注意：密码字段必须为 Argon2id PHC 字符串，直接填明文密码不会生效（设置密码推荐使用 `cgw set user` 或 WebUI）。

## 运行时指针文件 (`runtime.json`)

Daemon 启动后会在 `CGW_HOME`（`~/.compilegateway`）和 exe 同目录下写入临时的 `runtime.json`：

```json
{
  "pid": 12345,
  "url": "http://127.0.0.1:65251",
  "mcpUrl": "http://127.0.0.1:3994/mcp",
  "version": "0.1.0",
  "startedAt": 1775184000
}
```

CLI 客户端与 Node.js stdio 转发桥据此自动发现当前活跃的 Daemon 实例，无需人工指定端口。

## 日志与审计系统

日志文件存放在与 `config.json` 同级的 `logs/` 目录下：

- **按天滚动**：按 Daemon 宿主机的本地时间命名为 `logs/YYYY-MM-DD.log`。
- **单行格式化**：每条日志占一行（Tab 分隔），换行符与制表符均已转义，可直接使用 `grep` / `tail` 处理：
  ```text
  <ISO8601 时间戳>\t<级别>\t<作用域>\t<来源>\t<消息正文>
  ```
- **来源追踪**：通过 HTTP 请求头 `X-Client: webui | cli | mcp | daemon` 准确记录操作来源。
- **作用域隔离**：
  - 全局系统事件：`global`
  - 客户机 SSH 交互流：`client-<id>`
  - Job 执行控制台流：`job-<id>`

### 日志查看与检索

```bash
# 查看指定客户机的实时增量日志
cgw logs client c-mac --since 0

# 检索特定日期的历史归档日志（支持来源过滤）
cgw logs 2026-01-31 --source cli --limit 100

# 查看有哪些日期的日志可用
cgw logs days
```

超过 `logRetainDays` 的旧日志会在跨天写入或配置变更时被自动清理。

← [首页](Home.zh-cn)
