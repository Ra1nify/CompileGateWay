# 故障排查

本指南汇总了 CompileGateway 运行与使用过程中的常见故障症状、根本原因以及对应的修复方案。

---

## 1. 连接与主机密钥问题

### 主机指纹不匹配 (TOFU Host Key Mismatch)
- **症状**：`cgw clients test` 或执行任务时报错提示主机公钥指纹与已保存记录不一致。
- **原因**：初次信任（TOFU）机制记录了首次连接的主机公钥。目标机重装系统、重建虚拟机或更换 SSH 密钥导致指纹发生变化。
- **处置方案**：
  - 核对报错信息或 WebUI 弹窗中展示的对端实际出示指纹（`presentedKey`）。
  - 显式信任新指纹：
    ```bash
    cgw clients hostkey <clientId> --trust <SHA256:...>
    ```
  - 或清除历史记录（下次连接重新走初次信任）：
    ```bash
    cgw clients hostkey <clientId> --forget
    ```

### 私钥格式被拒（不支持传统加密 PEM）
- **症状**：Daemon 提示不支持使用 AES-CBC 加密的传统 PEM 私钥。
- **原因**：传统 OpenSSL PEM 加密（`EVP_BytesToKey` 配合 AES-CBC）属于陈旧格式，系统明确拒绝以保证密码学安全性。
- **处置方案**：使用 OpenSSL 一键转换为标准 PKCS#8 格式：
  ```bash
  openssl pkcs8 -topk8 -v2 aes-256-cbc -in old.key -out new.key
  ```

---

## 2. 源码同步与构建失败

### `chmod: gradlew: No such file or directory` / 远端项目目录为空
- **症状**：新登记的项目首次执行 `build` 任务时，在第一步立即报错文件不存在。
- **原因**：`build` 任务**只负责执行远程构建**，架构上**绝不会自动隐式推送**源码。
- **处置方案**：
  - 构建前先手动执行一次推送：
    ```bash
    cgw push <project> --wait
    ```
  - 或直接使用内置的 `push-build-pull` 跨项目工作流模板：
    ```bash
    cgw workflows apply <project> push-build-pull --apply --run --wait
    ```

### `/bin/sh^M: bad interpreter` 或 `\r: command not found`
- **症状**：Shell 脚本或 Gradle Wrapper 在远端 Unix 主机上执行报换行符错误。
- **原因**：Windows 检出代码带有 CRLF（`\r\n`）换行符。
- **处置方案**：CompileGateway 在执行 `push` 时会自动识别带 Shebang（`#!`）的脚本并转换为 LF。若此前指定了 `--keep-crlf`，去掉该参数重新推送即可：
  ```bash
  cgw push <project> --wait
  ```

### `artifactMissing` 产物缺失（HTTP 500）
- **症状**：构建步骤退出码为 0，但 Job 状态最终标记为 `failed`，错误码为 `artifactMissing`。
- **原因**：构建执行成功，但 Manifest 或任务声明的预期产物路径（`artifactPath` 或 `outputs`）在远端磁盘上并不存在。
- **处置方案**：
  - 使用 `cgw tree <project> build --side remote` 查看远端实际生成的产物路径与文件名。
  - 修改任务参数或 Manifest 中的 `artifactPath` 使其与真实路径匹配。

---

## 3. 并发冲突与队列排队

### `409 projectBusy`（CLI 退出码 4）
- **症状**：执行任务或 Batch 时报错 `409 projectBusy`。
- **原因**：该项目已有正在执行或排队中的 Job 占用了项目锁。
- **处置方案**：
  - 等待前序 Job 完成，或使用诊断命令查看当前任务状态：
    ```bash
    cgw doctor
    cgw jobs
    ```
  - 使用排队模式提交任务：
    ```bash
    cgw run <project> <task> --enqueue
    cgw batch run <project> <batch> --enqueue
    ```
  - 若前序任务异常挂起，可显式取消：
    ```bash
    cgw jobs cancel <jobId>
    ```

### `409 hostBusy`
- **症状**：执行任务报错 `409 hostBusy`。
- **原因**：配置中开启了全局 `hostLock`，且当前同一台物理客户机上已有其他项目的构建正在运行。
- **处置方案**：使用 `--enqueue` 排队等待主机释放，或关闭全局主机互斥锁：
  ```bash
  cgw set host-lock off
  ```

---

## 4. 端口与网络服务故障

### 启动时提示端口被占用
- **症状**：Daemon 启动失败并报错套接字绑定失败。
- **原因**：`config.json` 中配置的固定端口已被其他系统进程占用。
- **处置方案**：修改 `config.json` 将 `"port": 0` 改为由系统动态分配，或更换为其他空闲端口。

### MCP 端口冲突回退
- **症状**：日志提示默认 MCP 端口 3994 被占用。
- **机制说明**：Daemon 会自动退让并由系统分配空闲端口，实际端口写入 `runtime.json`。可随时通过以下命令查看当前实际 MCP 地址：
  ```bash
  cgw mcp url
  ```

---

## 5. 安全访问与鉴权排查

### `403 outsideRoot` 路径越界
- **症状**：浏览目录或登记项目时报错 403 `outsideRoot`。
- **原因**：访问的宿主机目录超出了用户主目录范围且不在 `allowedRoots` 白名单中。
- **处置方案**：将目标目录添加至受控白名单：
  ```bash
  cgw set roots add "D:/MyProjects"
  ```

### `429 authRateLimited` 登录限流
- **症状**：连续尝试登录报错 HTTP 429。
- **原因**：触发了基于来源 IP 的防爆破登录限流（`loginIntervalSeconds`）。
- **处置方案**：等待错误响应中返回的 `retryAfterSeconds` 秒数后再行尝试。

### 忘记管理密码
- **安全规则**：CompileGateway 的访问密码**只能在本机免密环境（127.0.0.1）下修改**。
- **处置方案**：直接在运行 Daemon 的宿主机上使用 CLI 或 WebUI 重置密码：
  ```bash
  cgw set user alice
  ```

← [首页](Home.zh-cn)
