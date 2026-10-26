# HTTP REST 接口

CompileGateway 在主服务端口提供完整的 RESTful HTTP/1.1 API（基于 Axum 实现）。WebUI、CLI 客户端（`cgw`）以及自定义调用方均通过这套统一接口与 Daemon 交互。

## 基础请求与请求头

- **默认基础 URL**：`http://127.0.0.1:<port>`（实际端口记录在 `runtime.json` 中）。
- **客户端来源请求头**：可在请求中携带 `X-Client: webui | cli | mcp | daemon` 用于日志审计来源追踪。
- **请求体格式**：`Content-Type: application/json`。

## 鉴权机制

| 请求来源 | 鉴权要求 | 机制 |
|---|---|---|
| 本机回环（`127.0.0.1` / `::1`） | 免认证 | 天然放行，开箱即用 |
| 局域网 / 公网（未配置密码） | 免认证 | 直接放行，但 WebUI/API 会返回安全警告 |
| 局域网 / 公网（已配置密码） | 必须认证 | 基于 Session Cookie 认证（`cgw_session`，12 小时有效，`HttpOnly` + `SameSite=Lax`） |

### 认证端点

- `POST /api/login` —— 用户名/密码登录，成功后在响应头 Set-Cookie 写入会话凭证：
  ```json
  { "username": "alice", "password": "secret" }
  ```
- `POST /api/logout` —— 销毁当前会话。

## 统一响应与错误模型

所有 API 均返回统一的 JSON 结构：

### 成功响应
```json
{ "ok": true, "data": { ... } }
```

### 错误响应
```json
{
  "ok": false,
  "error": {
    "code": "projectBusy",
    "httpStatus": 409,
    "message": "项目正在执行任务",
    "retryAfterSeconds": 5,
    "jobId": "j-1a2b",
    "projectId": "p-app"
  }
}
```

### 核心业务错误码

| 错误码 (`code`) | HTTP 状态码 | 含义说明 |
|---|---|---|
| `unauthorized` | 401 | 未登录或会话凭据失效 |
| `authLocalOnly` | 403 | 非本机尝试执行仅限本机修改的敏感安全接口 |
| `authRateLimited` | 429 | 登录失败触发按 IP 限流（返回 `retryAfterSeconds`） |
| `outsideRoot` | 403 | 访问路径越出 `allowedRoots` 允许的宿主根目录白名单 |
| `notFound` | 404 | 请求的客户机、项目、任务、Batch、Job 或 Manifest ID 不存在 |
| `projectBusy` | 409 | 该项目已有正在执行的 Job（返回正在运行的 `jobId` 与建议重试等待时间） |
| `hostBusy` | 409 | 全局 `hostLock` 打开且该客户机正在执行其他项目的任务 |
| `artifactMissing` | 500 | 构建执行成功（退出码 0），但声明的产物文件/目录未找到 |
| `workflowConflict` | 409 | 实例化工作流模板时发生任务/Batch ID 冲突且未加 `overwrite: true` |
| `workflowInvalid` | 400 | 工作流模板结构非法或引用了未声明的参数 |
| `manifestInvalid` | 400 | 构建类型 Manifest 结构非法或引用了未声明的占位符 |

---

## 端点列表 (Endpoints)

### 1. 系统状态与配置

- `GET /api/status` —— 获取 Daemon 运行状态、版本、端口、鉴权状态及安全警告标记。
- `GET /api/config` —— 读取当前完整配置。
- `PATCH /api/config` —— 动态修改配置项（传需修改的字段）。
- `GET /api/schema/{name}` —— 获取指定类型的 JSON Schema（`manifest` / `task` / `batch` / `client` / `project` / `workflow`）。

### 2. 客户机（SSH 目标机）与环境探测

- `GET /api/clients` —— 列出所有已登记的客户机。
- `POST /api/clients` —— 新增客户机配置。
- `PATCH /api/clients/{id}` —— 修改指定客户机配置。
- `DELETE /api/clients/{id}` —— 删除客户机。
- `POST /api/clients/{id}/test` —— 真实测试 SSH 连接连通性与 TOFU 主机指纹。
- `POST /api/clients/{id}/hostkey` —— 主机指纹管理（`trust` / `forget` / `import` / `export`）。
- `POST /api/clients/{id}/basedir` —— 远端创建并验证客户机基准目录（`mkdir -p && pwd`）。
- `GET /api/clients/{id}/env` —— 读取远端环境探测缓存结果。
- `POST /api/clients/{id}/env` —— 触发单次往返远程环境检测（Xcode、JDK、Rust 等）。

### 3. 源码目录浏览与项目管理

- `GET /api/browse?path=<absolute-path>` —— 浏览 Daemon 宿主机受控根目录白名单下的子文件夹。
- `GET /api/projects` —— 列出已登记的项目及当前运行状态。
- `POST /api/projects` —— 登记源码目录并绑定目标客户机。
- `DELETE /api/projects/{id}` —— 移除项目登记（保留本地与远程文件）。
- `GET /api/projects/{id}/tree?side=remote|host&path=<relative-path>` —— 遍历项目受控文件树。
- `GET /api/projects/{id}/cat?side=remote|host&path=<relative-path>` —— 受控预览文本文件内容。

### 4. 任务管理与执行

- `GET /api/projects/{id}/tasks` —— 列出项目下的所有任务。
- `POST /api/projects/{id}/tasks` —— 创建或保存任务定义。
- `PATCH /api/projects/{id}/tasks/{taskId}` —— 更新指定任务。
- `DELETE /api/projects/{id}/tasks/{taskId}` —— 删除任务。
- `POST /api/projects/{id}/tasks/{taskId}/preview` —— 预览渲染后的 Shell 构建命令与展开路径。
- `POST /api/projects/{id}/tasks/{taskId}/run` —— 立即执行任务（返回 `jobId`）。
- `POST /api/projects/{id}/tasks/{taskId}/enqueue` —— 将任务加入单项目 FIFO 排队队列。
- `POST /api/projects/{id}/push` —— 同步源码至目标机（`{ "clean": false, "keepCrlf": false }`）。
- `POST /api/projects/{id}/pull` —— 将远端产物拉取至宿主机托管目录。

### 5. 批量流水线与跨项目模板

- `GET /api/projects/{id}/batches` —— 列出项目内的批量工作流。
- `POST /api/projects/{id}/batches` —— 创建或保存批量工作流。
- `PATCH /api/projects/{id}/batches/{batchId}` —— 更新指定批量工作流。
- `DELETE /api/projects/{id}/batches/{batchId}` —— 删除批量工作流。
- `POST /api/projects/{id}/batches/{batchId}/run` —— 立即执行批量工作流。
- `POST /api/projects/{id}/batches/{batchId}/enqueue` —— 将批量工作流加入单项目 FIFO 队列。
- `GET /api/workflows` —— 列出跨项目工作流模板。
- `GET /api/workflows/{id}/raw` —— 获取工作流模板原始 JSON 文本。
- `PUT /api/workflows/{id}` —— 保存/更新工作流模板（`{ "raw": "<json>" }`）。
- `DELETE /api/workflows/{id}` —— 删除工作流模板文件。
- `POST /api/workflows/reset` —— 仅补齐缺失的内置工作流模板。
- `POST /api/projects/{id}/workflows/{preset}/apply` —— 将工作流模板实例化应用至项目。

### 6. 构建类型 (Manifests)

- `GET /api/manifests` —— 列出所有构建类型及其合法性。
- `GET /api/manifests/{id}/raw` —— 读取构建类型原始 JSON 文本。
- `PUT /api/manifests/{id}` —— 保存/更新构建类型 Manifest（`{ "raw": "<json>" }`）。
- `DELETE /api/manifests/{id}` —— 删除构建类型文件。
- `POST /api/manifests/reload` —— 重读磁盘 `manifest/` 目录并返回变更 Diff。
- `POST /api/manifests/detect` —— 按宿主机目录结构自动探测匹配的构建类型。

### 7. Job 执行与日志审计

- `GET /api/jobs` —— 列出最近 50 条 Job 执行记录。
- `GET /api/jobs/{id}` —— 查询指定 Job 详情、输出产物与步骤分步结果。
- `GET /api/jobs/{id}/logs` —— 读取指定 Job 的控制台日志（`?since=<line-number>`）。
- `POST /api/jobs/{id}/cancel` —— 协作式取消 Job。
- `POST /api/jobs/{id}/retry` —— 从失败步骤断点重试（`{ "fromStep": N }` 或 `{ "fromStart": true }`）。
- `GET /api/logs/days` —— 列出可用的历史归档日志日期。
- `GET /api/logs/date/{date}` —— 读取指定日期的归档日志（`?source=...&scope=...&limit=200`）。
- `GET /api/logs/client/{id}` —— 读取指定客户机的实时日志流。

### 8. 原生对话框代理

- `POST /api/dialog/pick-file` —— 在 Daemon 所在机器上唤起原生系统文件选择器（返回所选绝对路径）。
- `POST /api/dialog/pick-dir` —— 在 Daemon 所在机器上唤起原生系统目录选择器。

← [首页](Home.zh-cn)
