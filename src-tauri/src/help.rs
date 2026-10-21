//! The **single source** of the CLI command surface (design doc §3.1 / §3.2).
//!
//! Why it lives in the library and not in `bin/cli.rs`: one table feeds two consumers -
//!
//! * `cgw help ...` (the human tree / drill-down / WebUI table / `--json`);
//! * the MCP `cgw://cli` resource (an agent's view of the commands and exit codes).
//!
//! In the library both sides read one dataset, so a command cannot be added without
//! the other side seeing it.

use serde_json::json;

use crate::app::{APP_NAME, VERSION};
use crate::i18n::Lang;

/// Metadata for one command. Help text, `--json`, the WebUI table and `cgw://cli` **all come from
/// this table** - human text and `help --json` used to be two hand-written copies that drifted apart.
pub struct Help {
    /// Dot-separated path: `tasks.save` means `cgw tasks save`.
    pub name: &'static str,
    pub usage: &'static str,
    pub usage_en: &'static str,
    pub summary: &'static str,
    pub summary_en: &'static str,
    pub args: &'static [&'static str],
    pub args_en: &'static [&'static str],
    /// What this command maps to **in the WebUI** ("" = no UI entry).
    pub webui: &'static str,
    pub webui_en: &'static str,
}

impl Help {
    pub fn usage_for(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhHans => self.usage,
            Lang::EnUS => self.usage_en,
        }
    }

    pub fn summary_for(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhHans => self.summary,
            Lang::EnUS => self.summary_en,
        }
    }

    pub fn args_for(&self, lang: Lang) -> &'static [&'static str] {
        match lang {
            Lang::ZhHans => self.args,
            Lang::EnUS => self.args_en,
        }
    }

    pub fn webui_for(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhHans => self.webui,
            Lang::EnUS => self.webui_en,
        }
    }
}

macro_rules! help {
    ($name:literal, $usage:literal, $usage_en:literal, $summary:literal, $summary_en:literal, [$($arg:literal),*], [$($arg_en:literal),*], $webui:literal, $webui_en:literal) => {
        Help {
            name: $name,
            usage: $usage,
            usage_en: $usage_en,
            summary: $summary,
            summary_en: $summary_en,
            args: &[$($arg),*],
            args_en: &[$($arg_en),*],
            webui: $webui,
            webui_en: $webui_en,
        }
    };
}

pub const HELP: &[Help] = &[
    help!("doctor", "doctor", "doctor", "检查 daemon 可达性、登录状态、项目与 manifest 概况", "Check daemon reachability, auth status, projects, and manifests overview", [], [], "状态页顶部(daemon 在不在 / 要不要登录)", "Status page header (daemon reachability / auth prompt)"),
    help!("setup", "setup [--apply]", "setup [--apply]", "环境变量与 PATH:daemon 地址从哪来、cgw 在不在 PATH、怎么让它随处可用", "Environment variables and PATH setup for cgw", ["--apply"], ["--apply"], "(安装说明;Windows 上等价于把安装目录加进 PATH)", "(Setup guide; add binary directory to PATH)"),
    help!("status", "status", "status", "daemon 状态", "Daemon runtime status", [], [], "状态页", "Status page"),
    help!("login", "login [--username U] [--password P]", "login [--username U] [--password P]", "登录并保存会话", "Log in and save session", ["--username", "--password"], ["--username", "--password"], "登录页", "Login page"),
    help!("logout", "logout", "logout", "退出并删除本地会话", "Log out and remove local session", [], [], "登录页 → 退出", "Login page -> Log out"),
    help!("clients.list", "clients [list]", "clients [list]", "列出客户机", "List configured client machines", [], [], "客户机页(列表)", "Clients page (list)"),
    help!("clients.add", "clients add --host <ip> [--user U --password P | --key 私钥] [--name N] [--port P] [--tags a,b] [--base-dir 路径]", "clients add --host <ip> [--user U --password P | --key <path>] [--name N] [--port P] [--tags a,b] [--base-dir <path>]", "新建客户机", "Add a new client machine", ["--host", "--user", "--password", "--key", "--name", "--port", "--tags", "--base-dir"], ["--host", "--user", "--password", "--key", "--name", "--port", "--tags", "--base-dir"], "客户机页 → 新增", "Clients page -> Add Client"),
    help!("clients.edit", "clients edit <id> [--name N] [--host IP] [--port P] [--method password|key] [--user U] [--password P] [--key 路径] [--tags a,b] [--base-dir 路径]", "clients edit <id> [--name N] [--host IP] [--port P] [--method password|key] [--user U] [--password P] [--key <path>] [--tags a,b] [--base-dir <path>]", "修改客户机(只改给出的字段)", "Edit client machine properties", ["id", "--name", "--host", "--port", "--method", "--user", "--password", "--key", "--tags", "--base-dir"], ["id", "--name", "--host", "--port", "--method", "--user", "--password", "--key", "--tags", "--base-dir"], "客户机页 → 编辑", "Clients page -> Edit"),
    help!("clients.test", "clients test <id>", "clients test <id>", "真连一次 SSH;指纹不一致时会回传 presentedKey 与 mismatch", "Test SSH connection to client (reports host key mismatch)", ["id"], ["id"], "客户机页 → 测试连接(指纹不一致时会弹确认框)", "Clients page -> Test Connection"),
    help!("clients.hostkey", "clients hostkey <id> [--trust <SHA256:...> | --forget | --import <文件|-> [--force] | --export]", "clients hostkey <id> [--trust <SHA256:...> | --forget | --import <file|-> [--force] | --export]", "查看 / 信任 / 清除记录的主机指纹(TOFU),以及 known_hosts 的离线导入导出", "Manage host keys (TOFU trust, forget, known_hosts import/export)", ["id", "--trust", "--forget", "--import", "--force", "--export"], ["id", "--trust", "--forget", "--import", "--force", "--export"], "实例状态页 → 主机指纹(查看 / 信任 / 清除 / 从 known_hosts 导入 / 导出)", "Client status -> Host Key (trust, forget, import/export)"),
    help!("clients.rm", "clients rm <id>", "clients rm <id>", "删除客户机", "Delete client machine", ["id"], ["id"], "客户机页 → 删除", "Clients page -> Delete"),
    help!("basedir", "basedir <id> <路径> [--no-verify]", "basedir <id> <path> [--no-verify]", "设基准目录(默认连上去验证)", "Set base directory on client (verified via SSH by default)", ["id", "路径", "--no-verify"], ["id", "path", "--no-verify"], "实例状态页 → 基准目录(保存时真连上去验证)", "Client status -> Base Directory (verified via SSH on save)"),
    help!("env", "env <id> [--cached]", "env <id> [--cached]", "目标机环境检测(慢,最多 90 秒)", "Probe client environment toolchains (up to 90s)", ["id", "--cached"], ["id", "--cached"], "实例 → 环境页签", "Client -> Environment tab"),
    help!("env.script", "env --script", "env --script", "只打印要执行的检测脚本,不连机器", "Print environment probe script without connecting", ["--script"], ["--script"], "实例 → 环境页签(点检测前先看要跑什么)", "Client -> Environment tab (view probe script)"),
    help!("projects.list", "projects [list]", "projects [list]", "列出项目", "List registered projects", [], [], "状态页 → 项目列表", "Status page -> Project list"),
    help!("projects.add", "projects add --client <id> --path <绝对路径> [--name N]", "projects add --client <id> --path <path> [--name N]", "登记项目", "Register a local project directory", ["--client", "--path", "--name"], ["--client", "--path", "--name"], "状态页 → 导入项目(选 daemon 机器上的目录)", "Status page -> Import Project"),
    help!("projects.rm", "projects rm <id>", "projects rm <id>", "移除项目登记(不删文件)", "Unregister project (does not delete files)", ["id"], ["id"], "项目卡片 → 移除登记", "Project card -> Unregister"),
    help!("browse", "browse [绝对路径]", "browse [path]", "列 daemon 机器上的子文件夹(只在 allowedRoots 里:默认用户主目录)", "Browse host directories within allowed roots", ["path"], ["path"], "导入项目 / 选目录时的浏览框", "Directory browser modal"),
    help!("tree", "tree <project> [path] [--side remote|host]", "tree <project> [path] [--side remote|host]", "项目文件树的一层", "List project file tree level (host or remote)", ["project", "path", "--side"], ["project", "path", "--side"], "项目卡片 → 文件树", "Project card -> File Tree"),
    help!("cat", "cat <project> <路径> [--side remote|host]", "cat <project> <path> [--side remote|host]", "受控读一个文件(二进制只报元信息,大文件会截断)", "Read file contents (truncated if large, metadata only for binary)", ["project", "路径", "--side"], ["project", "path", "--side"], "文件树里点一个文件预览", "File tree preview modal"),
    help!("tasks.list", "tasks [list] <project>", "tasks [list] <project>", "列出项目的任务", "List tasks defined in a project", ["project"], ["project"], "项目卡片 → 任务列表", "Project card -> Task list"),
    help!("tasks.save", "tasks save <project> --body '<json>' | --file <路径> | -", "tasks save <project> --body '<json>' | --file <path> | -", "新建/修改任务(kind: build|push|pull|copy|move|delete|export)", "Create or update a task (build, push, pull, copy, move, delete, export)", ["project", "--body", "--file"], ["project", "--body", "--file"], "项目卡片 → 新建任务 / 编辑任务 / 编辑 JSON 参数", "Project card -> New/Edit Task modal"),
    help!("tasks.rm", "tasks rm <project> <taskId>", "tasks rm <project> <taskId>", "删除任务", "Delete a task", ["project", "taskId"], ["project", "taskId"], "任务行 → 删除", "Task row -> Delete"),
    help!("tasks.preview", "tasks preview <project> <taskId>", "tasks preview <project> <taskId>", "只渲染不执行", "Preview resolved task commands and parameters without running", ["project", "taskId"], ["project", "taskId"], "任务行 → 预览", "Task row -> Preview"),
    help!("run", "run <project> <taskId> [--wait] [--timeout N] [--enqueue]", "run <project> <taskId> [--wait] [--timeout N] [--enqueue]", "执行任务;--wait 会轮询到结束并以退出码反映成败;--enqueue = 项目忙时排队而不是报 projectBusy", "Execute a task (--wait polls until done, --enqueue queues if busy)", ["project", "taskId", "--wait", "--timeout", "--enqueue"], ["project", "taskId", "--wait", "--timeout", "--enqueue"], "任务行 → 执行(排队是界面没有的兜底)", "Task row -> Run (queueing via CLI)"),
    help!("batches", "batches <project>", "batches <project>", "列出批量工作流", "List batch workflows in a project", ["project"], ["project"], "项目卡片 → 批量工作流列表", "Project card -> Batch workflow list"),
    help!("batch.save", "batch save <project> <名字> <taskId...> | --body '<json>'", "batch save <project> <name> <taskId...> | --body '<json>'", "新建/修改批量工作流", "Create or update a batch workflow", ["project", "名字", "taskId", "--body", "--file"], ["project", "name", "taskId", "--body", "--file"], "批量工作流 → 新建 / 编辑 / 编辑 JSON", "Batch workflow -> New / Edit / Edit JSON"),
    help!("batch.run", "batch run <project> <batchId|名字> [--from N] [--wait] [--enqueue]", "batch run <project> <batchId|name> [--from N] [--wait] [--enqueue]", "执行批量工作流;--enqueue = 项目忙时排队", "Execute a batch workflow (--enqueue queues if busy)", ["project", "batchId", "--from", "--wait", "--enqueue"], ["project", "batchId", "--from", "--wait", "--enqueue"], "批量工作流 → 执行(排队是界面没有的兜底)", "Batch workflow -> Run"),
    help!("batch.rm", "batch rm <project> <batchId>", "batch rm <project> <batchId>", "删除批量工作流", "Delete a batch workflow", ["project", "batchId"], ["project", "batchId"], "批量工作流 → 删除", "Batch workflow -> Delete"),
    help!("push", "push <project> [--clean] [--keep-crlf] [--wait]", "push <project> [--clean] [--keep-crlf] [--wait]", "推送源码(默认将带 shebang 脚本换行转换为 LF,防止在 Unix/macOS 环境执行异常)", "Push source files to client (normalizes shebang scripts to LF)", ["project", "--clean", "--keep-crlf", "--wait"], ["project", "--clean", "--keep-crlf", "--wait"], "项目卡片 → 推送(弹窗二次确认,可勾选覆写)", "Project card -> Push (confirmation modal)"),
    help!("pull", "pull <project> [--wait]", "pull <project> [--wait]", "拉取产物", "Pull build artifacts from client to host", ["project", "--wait"], ["project", "--wait"], "项目卡片 → 回调(弹窗二次确认)", "Project card -> Pull (confirmation modal)"),
    help!("export", "export <project> [--source remote|host] [--path P] [--name N]", "export <project> [--source remote|host] [--path P] [--name N]", "导出快捷任务:打包归档并保存至宿主托管目录/exports", "Export quick task: package archive and save to exports directory", ["project", "--source", "--path", "--name"], ["project", "--source", "--path", "--name"], "项目卡片 → 导出(打包归档)", "Project card -> Export (archive modal)"),
    help!("exports", "exports <project> [名称 --save <路径>]", "exports <project> [name --save <path>]", "列出 / 下载已导出的压缩包", "List or download exported archive packages", ["project", "名称", "--save"], ["project", "name", "--save"], "项目卡片 → 导出对话框里的历史压缩包 + 下载", "Project card -> Export modal archive history"),
    help!("jobs.list", "jobs [list]", "jobs [list]", "最近 50 条 job", "List recent 50 jobs", [], [], "项目卡片的任务状态栏 / 最近任务", "Project card -> Recent jobs"),
    help!("job", "job <jobId>", "job <jobId>", "job 状态与每一步结果", "Job status and per-step execution details", ["jobId"], ["jobId"], "任务详情(每一步的结果)", "Job details drawer"),
    help!("jobs.logs", "jobs logs <jobId> [--since N]", "jobs logs <jobId> [--since N]", "job 日志", "Stream or read job logs", ["jobId", "--since"], ["jobId", "--since"], "任务详情里的日志", "Job details -> Logs"),
    help!("jobs.cancel", "jobs cancel <jobId>", "jobs cancel <jobId>", "取消 job", "Cancel a running job", ["jobId"], ["jobId"], "任务状态栏 → 取消", "Job status bar -> Cancel"),
    help!("jobs.retry", "jobs retry <jobId> [--from N|--from-start] [--wait]", "jobs retry <jobId> [--from N|--from-start] [--wait]", "从失败步骤重试", "Retry a failed job from the failed step or start", ["jobId", "--from", "--from-start", "--wait"], ["jobId", "--from", "--from-start", "--wait"], "失败后 → 从第 N 步重试", "Job failed -> Retry from step N"),
    help!("jobs.clear", "jobs clear [--project <id>]", "jobs clear [--project <id>]", "清空已完成的 job 历史", "Clear completed job history", ["--project"], ["--project"], "项目卡片 / 任务状态栏 → 清空历史", "Project card / Job bar -> Clear history"),
    help!("manifests.list", "manifests [list]", "manifests [list]", "列出构建类型", "List available build manifests", [], [], "构建类型页", "Manifests page"),
    help!("manifests.show", "manifests show <id>", "manifests show <id>", "打印 manifest 原文", "Print raw manifest JSON content", ["id"], ["id"], "构建类型页 → 编辑原始 JSON", "Manifests page -> Raw JSON"),
    help!("manifests.schema", "manifests schema", "manifests schema", "同 `cgw schema manifest`", "Alias of `cgw schema manifest`", [], [], "", ""),
    help!("schema", "schema [manifest|task|batch|client|project]", "schema [manifest|task|batch|client|project]", "从 Rust 结构体生成的 JSON Schema(draft-07;不带名字则列出可用的)", "Generated JSON Schemas from Rust types (draft-07)", ["名称"], ["name"], "", ""),
    help!("manifests.save", "manifests save <id> --file <路径> | -", "manifests save <id> --file <path> | -", "保存 manifest(校验不过会被拒)", "Save or update a manifest (validated against schema)", ["id", "--file"], ["id", "--file"], "构建类型页 → 保存", "Manifests page -> Save"),
    help!("manifests.status", "manifests status", "manifests status", "目录在哪、共几份、哪几份不合法、自带模板缺哪些", "Manifest directory status, counts, invalid files, and missing presets", [], [], "构建类型页(顶部状态)", "Manifests page -> Status overview"),
    help!("manifests.import", "manifests import <文件.json> [--id <名称>] [--force]", "manifests import <file.json> [--id <name>] [--force]", "从本地文件导入一份构建类型(id 默认取文件名)", "Import a manifest from a local JSON file", ["文件", "--id", "--force"], ["file", "--id", "--force"], "构建类型页 → 导入", "Manifests page -> Import"),
    help!("manifests.export", "manifests export <id> [--save <路径>]", "manifests export <id> [--save <path>]", "导出一份构建类型(不写 --save 就打到标准输出)", "Export a manifest to stdout or a file", ["id", "--save"], ["id", "--save"], "构建类型页 → 导出", "Manifests page -> Export"),
    help!("manifests.templates", "manifests templates", "manifests templates", "检查当前缺失的内置预设模板清单", "List missing built-in preset templates", [], [], "构建类型页 → 还缺哪些模板", "Manifests page -> Missing templates"),
    help!("manifests.rm", "manifests rm <id>", "manifests rm <id>", "删除构建类型", "Delete a manifest", ["id"], ["id"], "构建类型页 → 删除", "Manifests page -> Delete"),
    help!("manifests.reset", "manifests reset [id...]", "manifests reset [id...]", "仅补齐缺失的预设模板文件(保留已有修改)", "Restore missing preset templates without overwriting existing ones", ["id"], ["id"], "构建类型页 → 补齐缺失的模板", "Manifests page -> Restore presets"),
    help!("manifests.detect", "manifests detect <宿主目录>", "manifests detect <path>", "按目录探测命中的类型", "Detect matching manifest for a project directory", ["目录"], ["dir"], "构建类型页 → 按目录探测", "Manifests page -> Detect by path"),
    help!("manifests.refresh", "manifests refresh", "manifests refresh", "从磁盘重读构建类型目录(外部修改后手动同步,免去后台监听开销),并报告增删改详情", "Reload manifest directory from disk and report changes", [], [], "构建类型页 → 刷新", "Manifests page -> Refresh"),
    help!("workflows.list", "workflows [list]", "workflows [list]", "列出跨项目工作流模板", "List cross-project workflow templates", [], [], "工作流页", "Workflows page"),
    help!("workflows.show", "workflows show <id>", "workflows show <id>", "打印模板原文 + 解析结果", "Print workflow template JSON and resolved tasks", ["id"], ["id"], "工作流页 → 查看模板", "Workflows page -> View template"),
    help!("workflows.save", "workflows save <id> --body '<json>' | --file <路径> | -", "workflows save <id> --body '<json>' | --file <path> | -", "新建/修改工作流模板(校验不过会被拒)", "Create or update a workflow template", ["id", "--body", "--file"], ["id", "--body", "--file"], "工作流页 → 编辑模板", "Workflows page -> Edit template"),
    help!("workflows.rm", "workflows rm <id>", "workflows rm <id>", "删除工作流模板(已 apply 到项目的不受影响)", "Delete workflow template (applied tasks remain intact)", ["id"], ["id"], "工作流页 → 删除", "Workflows page -> Delete"),
    help!("workflows.apply", "workflows apply <project> <id> [--set k=v ...] [--apply] [--overwrite] [--run] [--name N]", "workflows apply <project> <id> [--set k=v ...] [--apply] [--overwrite] [--run] [--name N]", "把模板实例化进项目;默认只预览任务清单与冲突,确认后加 --apply", "Instantiate template into project (--apply to persist)", ["project", "id", "--set", "--apply", "--overwrite", "--run", "--name"], ["project", "id", "--set", "--apply", "--overwrite", "--run", "--name"], "工作流页 → 应用到项目", "Workflows page -> Apply to project"),
    help!("workflows.refresh", "workflows refresh", "workflows refresh", "仅恢复缺失的内置工作流模板(保留已有修改)", "Restore missing built-in workflow templates", [], [], "工作流页 → 补齐模板", "Workflows page -> Restore built-in templates"),
    help!("logs.days", "logs [days]", "logs [days]", "有哪些天的落盘日志", "List dates with persisted logs", [], [], "实例 → 日志页签(按天选历史)", "Client -> Logs tab (date selector)"),
    help!("logs.day", "logs <YYYY-MM-DD> [--source S] [--scope X] [--limit N]", "logs <YYYY-MM-DD> [--source S] [--scope X] [--limit N]", "读某一天的落盘日志", "Read persisted log lines for a specific date", ["日期", "--source", "--scope", "--limit"], ["date", "--source", "--scope", "--limit"], "实例 → 日志页签(历史那一天)", "Client -> Logs tab (historical logs)"),
    help!("logs.client", "logs client <id> [--since N]", "logs client <id> [--since N]", "某台客户机的实时日志", "Stream live logs for a client machine", ["id", "--since"], ["id", "--since"], "实例 → 日志页签(实时)", "Client -> Logs tab (live stream)"),
    help!("config.show", "config [show]", "config [show]", "查看配置", "Show daemon configuration", [], [], "配置页", "Settings page"),
    help!("config.set", "config set [--port N] [--listen lan|wan] [--language L] [--login-interval S] [--log-retain-days N] [--username U --password P] [--allowed-roots 路径;路径] [--mcp-enabled on|off] [--mcp-port N] [--mcp-listen lan|wan]", "config set [--port N] [--listen lan|wan] [--language L] [--login-interval S] [--log-retain-days N] [--username U --password P] [--allowed-roots path;path] [--mcp-enabled on|off] [--mcp-port N] [--mcp-listen lan|wan]", "改配置(只改给出的字段;--mcp-* 与 --allowed-roots 立即生效,其余端口项要重启)", "Update configuration fields", ["--port", "--listen", "--language", "--login-interval", "--log-retain-days", "--username", "--password", "--allowed-roots", "--mcp-enabled", "--mcp-port", "--mcp-listen"], ["--port", "--listen", "--language", "--login-interval", "--log-retain-days", "--username", "--password", "--allowed-roots", "--mcp-enabled", "--mcp-port", "--mcp-listen"], "配置页 → 保存", "Settings page -> Save"),
    help!("set.port", "set port --webui N | --mcp N", "set port --webui N | --mcp N", "改端口(WebUI 与 MCP 是两个独立端口)", "Set WebUI or MCP service port", ["--webui", "--mcp"], ["--webui", "--mcp"], "配置页 → 端口", "Settings page -> Ports"),
    help!("set.listen", "set listen --webui lan|wan | --mcp lan|wan", "set listen --webui lan|wan | --mcp lan|wan", "改监听范围(只监听本机 / 允许其他设备)", "Set network listen scope (lan or wan)", ["--webui", "--mcp"], ["--webui", "--mcp"], "配置页 → 监听模式", "Settings page -> Listen mode"),
    help!("set.user", "set user --name U [--password P] | --clear", "set user --name U [--password P] | --clear", "改访问凭据(只有本机可改);--clear 清掉", "Set or clear daemon access credentials", ["--name", "--password", "--clear"], ["--name", "--password", "--clear"], "配置页 → 访问凭据", "Settings page -> Credentials"),
    help!("set.language", "set language auto|zh-Hans|en-US", "set language auto|zh-Hans|en-US", "界面与日志语言", "Set UI and log language (auto, zh-Hans, en-US)", ["值"], ["value"], "配置页 → 语言", "Settings page -> Language"),
    help!("set.login-interval", "set login-interval <秒>", "set login-interval <seconds>", "登录限流间隔(0 = 不限流)", "Set login rate-limit throttle interval in seconds", ["秒"], ["seconds"], "配置页 → 登录限流", "Settings page -> Login throttle"),
    help!("set.log-retain", "set log-retain <天数>", "set log-retain <days>", "日志保留天数", "Set log retention period in days", ["天数"], ["days"], "配置页 → 日志保留天数", "Settings page -> Log retention"),
    help!("set.roots", "set roots [list] | add|rm|set <绝对路径...>", "set roots [list] | add|rm|set <path...>", "宿主目录白名单(browse/登记项目/探测共用)", "Manage allowed host directories whitelist", ["动作", "路径"], ["action", "path"], "配置页 → 额外允许的宿主目录", "Settings page -> Allowed root paths"),
    help!("set.mcp", "set mcp on|off", "set mcp on|off", "MCP 开关(同 `cgw mcp enable|disable`)", "Enable or disable MCP server", ["on|off"], ["on|off"], "配置页 → MCP → 启用", "Settings page -> MCP -> Enable"),
    help!("set.host-lock", "set host-lock on|off", "set host-lock on|off", "主机级互斥:同一台客户机一次只跑一个 job(立即生效)", "Host-level concurrency lock: run at most one job per client", ["on|off"], ["on|off"], "配置页 → 同一台客户机一次只跑一个任务", "Settings page -> Host-level concurrency lock"),
    help!("set.all", "set all [--json]", "set all [--json]", "把配置页上能改的东西全列出来(带改法)", "List all configurable settings and their usage", [], [], "配置页(整页)", "Settings page (all options)"),
    help!("mcp.status", "mcp [status]", "mcp [status]", "MCP 适配层:开关、监听、端口、客户端该填的 URL、工具数", "MCP status: state, listen scope, port, endpoint URL, tool count", [], [], "配置页 → MCP(AI agent)段", "Settings page -> MCP section"),
    help!("mcp.url", "mcp url", "mcp url", "只打印 MCP 端点 URL(给脚本 / 客户端配置用)", "Print MCP endpoint URL for client config", [], [], "配置页 → MCP 段的端点地址", "Settings page -> MCP endpoint URL"),
    help!("mcp.tools", "mcp tools [名称]", "mcp tools [name]", "列出 MCP 工具(和 agent 看到的是同一份);带名字则打印它的参数 schema", "List MCP tools or inspect a tool parameter schema", [], [], "配置页 → MCP 段的工具数", "Settings page -> MCP tool count"),
    help!("mcp.enable", "mcp enable | mcp disable", "mcp enable | mcp disable", "开 / 关 MCP(立即生效,不用重启;on / off 也认)", "Enable or disable MCP server dynamically", ["enable|disable"], ["enable|disable"], "配置页 → MCP → 启用", "Settings page -> MCP toggle"),
    help!("mcp.port", "mcp port <N>", "mcp port <N>", "改 MCP 端口(0 = 自动挑;立即生效)", "Change MCP port (0 for dynamic port)", ["N"], ["N"], "配置页 → MCP → 端口", "Settings page -> MCP port"),
    help!("mcp.listen", "mcp listen lan|wan", "mcp listen lan|wan", "改 MCP 的监听范围(立即生效)", "Change MCP network listen scope (lan or wan)", ["lan|wan"], ["lan|wan"], "配置页 → MCP → 监听", "Settings page -> MCP listen scope"),
    help!("api", "api <METHOD> <PATH> [JSON]", "api <METHOD> <PATH> [JSON]", "直接调 HTTP API(兜底,能打到任何接口)", "Call daemon HTTP API endpoint directly", ["METHOD", "PATH", "JSON"], ["METHOD", "PATH", "JSON"], "(兜底:WebUI 的每个接口都能这么打)", "(Direct HTTP API client)"),
    help!("help", "help [命令] [子命令] | --webui | --json", "help [command] [subcommand] | --webui | --json", "逐层展开的帮助;--webui 打 WebUI 对照表,--json 给 agent", "Command help tree; --webui for UI mapping, --json for machine parsing", [], [], "", ""),
];

/// Top-level command -> one-line blurb for the group (the skeleton of the help tree).
pub const GROUPS: &[(&str, &str, &str)] = &[
    ("doctor", "自检", "System diagnostic check"),
    ("setup", "环境变量与 PATH(让 cgw 随处可用)", "Environment variables and PATH setup"),
    ("status", "daemon 状态", "Daemon runtime status"),
    ("login", "登录(也可用 $CGW_USERNAME/$CGW_PASSWORD)", "Log in (or use $CGW_USERNAME / $CGW_PASSWORD)"),
    ("logout", "退出并删除本地会话", "Log out and remove local session"),
    ("clients", "客户机(SSH 目标机)与主机指纹", "Client machines (SSH targets) and host keys"),
    ("basedir", "客户机上的基准目录", "Client base directory configuration"),
    ("env", "目标机环境依赖检测", "Client environment toolchain probe"),
    ("projects", "项目登记", "Project registration"),
    ("browse", "浏览服务端宿主目录", "Browse server host directories"),
    ("tree", "项目文件树浏览", "Project file tree navigation"),
    ("cat", "受控读取文件内容与元信息", "Controlled file read and metadata preview"),
    ("tasks", "项目任务(七种类型)", "Project tasks (7 types)"),
    ("run", "执行单个任务(--wait 跟到结束)", "Run a task (--wait to follow until completion)"),
    ("batches", "批量工作流(列表)", "Batch workflows (list)"),
    ("batch", "批量工作流(保存 / 执行 / 删除)", "Batch workflows (save / run / delete)"),
    ("push", "推送源码", "Push source code to client"),
    ("pull", "把产物拉回宿主托管目录", "Pull build artifacts back to host"),
    ("export", "导出快捷任务:打包归档放进宿主托管目录", "Export quick task: package archive to host"),
    ("exports", "已导出的压缩包(列表 / 下载)", "Exported archives (list / download)"),
    ("jobs", "任务(job)列表与日志、取消、重试", "Job history, logs, cancellation, and retry"),
    ("job", "单个任务(job)的详情", "Single job details"),
    ("manifests", "构建类型 (Manifest) 管理", "Build manifests management"),
    ("workflows", "跨项目工作流模板", "Cross-project workflow templates"),
    ("schema", "从 Rust 结构体生成的 JSON Schema", "JSON Schemas generated from Rust types"),
    ("logs", "按天持久化日志查看", "Persisted log viewer by date"),
    ("config", "配置(扁平写法)", "Configuration (flat syntax)"),
    ("set", "配置(分类写法,和配置页一一对应)", "Configuration (categorized syntax)"),
    ("mcp", "MCP 适配层(给 AI agent 用;daemon 内置,跑在另一个端口上)", "MCP server for AI agents (runs on a dedicated port)"),
    ("api", "兜底:直接打 HTTP 接口", "Call HTTP API directly"),
    ("help", "帮助本身(逐层展开)", "Interactive help tree"),
];

/// Global flags (shared by help and `--json`).
pub const GLOBAL_FLAGS: &[&str] = &[
    "--url <url>",
    "--json",
    "--wait",
    "--timeout <秒>",
    "--help",
    "--version",
];

pub const GLOBAL_FLAGS_EN: &[&str] = &[
    "--url <url>",
    "--json",
    "--wait",
    "--timeout <seconds>",
    "--help",
    "--version",
];

pub fn global_flags_for(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::ZhHans => GLOBAL_FLAGS,
        Lang::EnUS => GLOBAL_FLAGS_EN,
    }
}

/// Exit code contract (design doc §3.2). Scripts and agents **judge by this, not by wording**.
pub const EXIT_CODES: &[(u8, &str, &str)] = &[
    (0, "成功", "Success"),
    (1, "用法/参数错", "Usage or argument error"),
    (2, "连不上 daemon", "Cannot connect to daemon"),
    (3, "daemon 返回错误", "Daemon returned error"),
    (4, "项目忙(已有 job 在跑,HTTP 409 projectBusy)", "Project busy (another job is running, HTTP 409)"),
    (5, "--wait 等的那个 job 失败了", "The job monitored by --wait failed"),
    (6, "目标不存在(HTTP 404)", "Target not found (HTTP 404)"),
    (7, "需要登录(HTTP 401;403 里只有 authLocalOnly 算这一档,别的 403 是 3)", "Authentication required (HTTP 401)"),
];

/// One-line summary of the exit codes (for humans).
pub fn exit_code_help() -> String {
    exit_code_help_for(crate::i18n::current())
}

pub fn exit_code_help_for(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "0 成功 / 1 用法错 / 2 连不上 / 3 接口错 / 4 项目忙 / 5 job 失败 / 6 找不到 / 7 需要登录".to_string(),
        Lang::EnUS => "0 success / 1 usage error / 2 cannot connect / 3 daemon error / 4 project busy / 5 job failed / 6 not found / 7 auth required".to_string(),
    }
}

/// Ordered, de-duplicated list of top-level names (the first segment of a dotted path).
pub fn top_level_names() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for entry in HELP {
        let top = entry.name.split('.').next().unwrap_or(entry.name);
        if !out.contains(&top) {
            out.push(top);
        }
    }
    out
}

/// Subcommands under a path (direct children, not recursive).
pub fn children_of(path: &str) -> Vec<&'static Help> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    HELP.iter()
        .filter(|entry| {
            entry
                .name
                .strip_prefix(&prefix)
                .map(|rest| !rest.is_empty() && !rest.contains('.'))
                .unwrap_or(false)
        })
        .collect()
}

/// Find one exact command (`tasks.save`).
pub fn find_command(path: &str) -> Option<&'static Help> {
    HELP.iter().find(|entry| entry.name == path)
}

/// Turn typed positional words into a command path: `["help","tasks","save"]` -> `tasks.save`.
pub fn help_path(words: &[String]) -> String {
    words
        .iter()
        .map(String::as_str)
        .filter(|word| !matches!(*word, "help" | "?"))
        .take_while(|word| !word.starts_with('-'))
        .collect::<Vec<_>>()
        .join(".")
}

/// Result of resolving a `cgw help <path>` request.
pub enum Resolved {
    /// Nothing given: print the whole tree.
    Tree(String),
    /// Matched one command.
    Command(String),
    /// Matched a group of subcommands.
    Group(String),
    /// No match - carries the names usable at this level for the caller to report as a usage error.
    Unknown { path: String, hints: Vec<String> },
}

/// Resolve one help request using the active system language.
pub fn resolve(words: &[String]) -> Resolved {
    resolve_for(words, crate::i18n::current())
}

/// Resolve one help request for a specific language.
pub fn resolve_for(words: &[String], lang: Lang) -> Resolved {
    let path = help_path(words);
    if path.is_empty() {
        return Resolved::Tree(tree_for(lang));
    }
    if let Some(entry) = find_command(&path) {
        return Resolved::Command(command_for(entry, lang));
    }
    let children = children_of(&path);
    if !children.is_empty() {
        return Resolved::Group(group_for(&path, &children, lang));
    }
    let parent = path.rsplit_once('.').map(|(head, _)| head).unwrap_or("");
    let hints: Vec<String> = if parent.is_empty() {
        top_level_names().into_iter().map(str::to_string).collect()
    } else {
        children_of(parent)
            .iter()
            .map(|entry| entry.name.to_string())
            .collect()
    };
    Resolved::Unknown { path, hints }
}

/// `cgw help` (no path): a **layered** command tree.
pub fn tree() -> String {
    tree_for(crate::i18n::current())
}

pub fn tree_for(lang: Lang) -> String {
    let mut out = String::new();
    match lang {
        Lang::ZhHans => {
            out.push_str(&format!("{APP_NAME} {VERSION} —— daemon 的 HTTP 瘦客户端\n\n"));
            out.push_str("用法: cgw <命令> [子命令...] [参数] [--url http://host:port] [--json] [--wait]\n\n");
            out.push_str("  `cgw help <命令> [子命令]` 逐层展开;`cgw <命令> -h` 等价。\n");
            out.push_str("  `cgw help --webui` 看每条命令对应界面上的哪个位置(WebUI 能做的 CLI 都能做)。\n");
            out.push_str("  `cgw help --json` 给 agent 一份机器可读的命令面。\n\n");

            for (top, blurb_zh, _) in GROUPS {
                if find_command(top).is_some() {
                    out.push_str(&format!("  {top:<10} {blurb_zh}\n"));
                    continue;
                }
                let children = children_of(top);
                let names = children
                    .iter()
                    .filter_map(|entry| entry.name.split('.').nth(1))
                    .collect::<Vec<_>>()
                    .join(" / ");
                out.push_str(&format!("  {top:<10} {blurb_zh}  —— {names}\n"));
            }

            out.push_str(&format!("\n全局参数:{}\n", global_flags_for(lang).join("  ")));
            out.push_str(&format!("退出码:{}\n", exit_code_help_for(lang)));
            out.push_str("地址解析:--url / $CGW_URL → daemon 写的 runtime.json → config.json");
        }
        Lang::EnUS => {
            out.push_str(&format!("{APP_NAME} {VERSION} — HTTP thin client for daemon\n\n"));
            out.push_str("Usage: cgw <command> [subcommand...] [arguments] [--url http://host:port] [--json] [--wait]\n\n");
            out.push_str("  `cgw help <command> [subcommand]` drills down; `cgw <command> -h` is equivalent.\n");
            out.push_str("  `cgw help --webui` maps each command to its WebUI location (full WebUI parity).\n");
            out.push_str("  `cgw help --json` produces a machine-readable command schema for agents.\n\n");

            for (top, _, blurb_en) in GROUPS {
                if find_command(top).is_some() {
                    out.push_str(&format!("  {top:<10} {blurb_en}\n"));
                    continue;
                }
                let children = children_of(top);
                let names = children
                    .iter()
                    .filter_map(|entry| entry.name.split('.').nth(1))
                    .collect::<Vec<_>>()
                    .join(" / ");
                out.push_str(&format!("  {top:<10} {blurb_en}  — {names}\n"));
            }

            out.push_str(&format!("\nGlobal flags:{}\n", global_flags_for(lang).join("  ")));
            out.push_str(&format!("Exit codes:{}\n", exit_code_help_for(lang)));
            out.push_str("Address resolution: --url / $CGW_URL -> runtime.json written by daemon -> config.json");
        }
    }
    out
}

/// Subcommand list for a command group (`cgw help tasks`).
pub fn group(path: &str, children: &[&'static Help]) -> String {
    group_for(path, children, crate::i18n::current())
}

pub fn group_for(path: &str, children: &[&'static Help], lang: Lang) -> String {
    let mut out = String::new();
    let blurb = GROUPS
        .iter()
        .find(|(name, _, _)| *name == path)
        .map(|(_, zh, en)| match lang {
            Lang::ZhHans => *zh,
            Lang::EnUS => *en,
        })
        .unwrap_or_else(|| match lang {
            Lang::ZhHans => "子命令",
            Lang::EnUS => "Subcommands",
        });

    let sep = match lang {
        Lang::ZhHans => "——",
        Lang::EnUS => "—",
    };
    out.push_str(&format!("cgw {path} {sep} {blurb}\n\n"));
    for entry in children {
        let leaf = entry.name.rsplit('.').next().unwrap_or(entry.name);
        out.push_str(&format!("  {leaf:<14} {}\n", entry.summary_for(lang)));
        out.push_str(&format!("  {:<14} {}\n", "", entry.usage_for(lang)));
        let webui = entry.webui_for(lang);
        if !webui.is_empty() {
            out.push_str(&format!("  {:<14} WebUI: {webui}\n", ""));
        }
        out.push('\n');
    }
    match lang {
        Lang::ZhHans => out.push_str(&format!("看某个子命令的参数:`cgw help {path} <子命令>`")),
        Lang::EnUS => out.push_str(&format!("Inspect subcommand parameters: `cgw help {path} <subcommand>`")),
    }
    out
}

/// Details of one command (`cgw help tasks save`).
pub fn command(entry: &Help) -> String {
    command_for(entry, crate::i18n::current())
}

pub fn command_for(entry: &Help, lang: Lang) -> String {
    let mut out = String::new();
    let sep = match lang {
        Lang::ZhHans => "——",
        Lang::EnUS => "—",
    };
    out.push_str(&format!(
        "cgw {} {sep} {}\n\n",
        entry.name.replace('.', " "),
        entry.summary_for(lang)
    ));
    let args = entry.args_for(lang);
    match lang {
        Lang::ZhHans => {
            out.push_str(&format!("用法: cgw {}\n", entry.usage));
            if args.is_empty() {
                out.push_str("参数:无\n");
            } else {
                out.push_str(&format!("参数:{}\n", args.join("  ")));
            }
            out.push_str(&format!(
                "WebUI 对应:{}\n",
                if entry.webui.is_empty() {
                    "没有对应界面(CLI / 接口独有)"
                } else {
                    entry.webui
                }
            ));
            if entry.name.contains('.') {
                let parent = entry.name.rsplit_once('.').map(|(head, _)| head).unwrap_or("");
                out.push_str(&format!("同组其它子命令:`cgw help {parent}`\n"));
            }
            out.push_str("机器可读形态:`cgw help --json`(字段名和 MCP 工具对齐)");
        }
        Lang::EnUS => {
            out.push_str(&format!("Usage: cgw {}\n", entry.usage_en));
            if args.is_empty() {
                out.push_str("Arguments: none\n");
            } else {
                out.push_str(&format!("Arguments: {}\n", args.join("  ")));
            }
            out.push_str(&format!(
                "WebUI mapping: {}\n",
                if entry.webui_en.is_empty() {
                    "No UI entry (CLI / API exclusive)"
                } else {
                    entry.webui_en
                }
            ));
            if entry.name.contains('.') {
                let parent = entry.name.rsplit_once('.').map(|(head, _)| head).unwrap_or("");
                out.push_str(&format!("Other subcommands in group: `cgw help {parent}`\n"));
            }
            out.push_str("Machine-readable schema: `cgw help --json` (fields aligned with MCP tools)");
        }
    }
    out
}

/// Command <-> WebUI mapping table (`cgw help --webui`).
pub fn webui_table() -> String {
    webui_table_for(crate::i18n::current())
}

pub fn webui_table_for(lang: Lang) -> String {
    let mut out = String::new();
    match lang {
        Lang::ZhHans => {
            out.push_str("WebUI 上的位置  →  cgw 命令\n\n");
            for entry in HELP.iter().filter(|entry| !entry.webui.is_empty()) {
                out.push_str(&format!("  {:<52} cgw {}\n", entry.webui, entry.usage));
            }
            let with_ui = HELP.iter().filter(|entry| !entry.webui.is_empty()).count();
            let without = HELP.iter().filter(|entry| entry.webui.is_empty()).count();
            out.push_str(&format!(
                "\n上面 {with_ui} 条覆盖了界面上的全部入口;剩下 {without} 条是界面没有的(给脚本 / agent 用)。\n"
            ));
            out.push_str("反过来也成立:CLI 的每个动作最终都落到项目 JSON 里的任务或 batch,界面不维护隐式动作。");
        }
        Lang::EnUS => {
            out.push_str("WebUI Location  ->  cgw Command\n\n");
            for entry in HELP.iter().filter(|entry| !entry.webui_en.is_empty()) {
                out.push_str(&format!("  {:<52} cgw {}\n", entry.webui_en, entry.usage_en));
            }
            let with_ui = HELP.iter().filter(|entry| !entry.webui_en.is_empty()).count();
            let without = HELP.iter().filter(|entry| entry.webui_en.is_empty()).count();
            out.push_str(&format!(
                "\nThe {with_ui} entries above cover all UI interactions; remaining {without} entries are CLI/script-only.\n"
            ));
            out.push_str("The converse also holds: every CLI action maps to a task/batch in project JSON; UI maintains no implicit actions.");
        }
    }
    out
}

/// Machine-readable command surface (design doc §3.1).
pub fn json() -> String {
    json_for(crate::i18n::current())
}

pub fn json_for(lang: Lang) -> String {
    let commands: Vec<serde_json::Value> = HELP
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "usage": entry.usage_for(lang),
                "summary": entry.summary_for(lang),
                "args": entry.args_for(lang),
                "webui": entry.webui_for(lang),
            })
        })
        .collect();

    let webui: Vec<serde_json::Value> = HELP
        .iter()
        .filter(|entry| !entry.webui_for(lang).is_empty())
        .map(|entry| json!({ "webui": entry.webui_for(lang), "command": entry.name }))
        .collect();

    let exit_codes: serde_json::Map<String, serde_json::Value> = EXIT_CODES
        .iter()
        .map(|(code, zh, en)| {
            let meaning = match lang {
                Lang::ZhHans => *zh,
                Lang::EnUS => *en,
            };
            (code.to_string(), json!(meaning))
        })
        .collect();

    let (usage, help_tree, help_webui, help_json, env_url, env_user, env_pass) = match lang {
        Lang::ZhHans => (
            "cgw <命令> [子命令...] [参数] [--url http://host:port] [--json] [--wait]",
            "cgw help [命令] [子命令]",
            "cgw help --webui",
            "cgw help --json",
            "daemon 地址;也可以写进 runtime.json",
            "需要登录时的用户名(配合 CGW_PASSWORD,不留状态在磁盘上)",
            "需要登录时的密码",
        ),
        Lang::EnUS => (
            "cgw <command> [subcommand...] [arguments] [--url http://host:port] [--json] [--wait]",
            "cgw help [command] [subcommand]",
            "cgw help --webui",
            "cgw help --json",
            "daemon address; can also be stored in runtime.json",
            "username when authentication is required (paired with CGW_PASSWORD)",
            "password when authentication is required",
        ),
    };

    serde_json::to_string_pretty(&json!({
        "app": APP_NAME,
        "version": VERSION,
        "usage": usage,
        "help": {
            "tree": help_tree,
            "webuiTable": help_webui,
            "json": help_json,
        },
        "env": {
            "CGW_URL": env_url,
            "CGW_USERNAME": env_user,
            "CGW_PASSWORD": env_pass,
        },
        "exitCodes": exit_codes,
        "globalFlags": global_flags_for(lang),
        "commands": commands,
        "webuiParity": webui,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two commands on one level must not share a name - a duplicate makes exact lookup pick the first.
    #[test]
    fn names_are_unique_and_usage_matches_the_path() {
        let mut seen = std::collections::HashSet::new();
        for entry in HELP {
            assert!(seen.insert(entry.name), "命令名重复:{}", entry.name);
            assert!(!entry.summary.trim().is_empty(), "{} 没有中文说明", entry.name);
            assert!(!entry.summary_en.trim().is_empty(), "{} 没有英文说明", entry.name);
        }

        for entry in HELP {
            let first = entry.usage.split_whitespace().next().unwrap_or("");
            let first_name = entry.name.split('.').next().unwrap_or("");
            assert_eq!(
                first, first_name,
                "{} 的 usage 以 {first} 开头,和命令名对不上",
                entry.name
            );
            if let Some(leaf) = entry.name.split('.').nth(1) {
                let bare: [&str; 2] = ["list", "day"];
                let mentions = entry.usage.contains(leaf) || bare.contains(&leaf);
                assert!(
                    mentions,
                    "{} 的 usage(`{}`)里没提到子命令名 {leaf}",
                    entry.name, entry.usage
                );
            }
        }
    }

    /// Every top-level command needs a GROUPS blurb.
    #[test]
    fn tree_covers_every_top_level_command() {
        let groups: Vec<&str> = GROUPS.iter().map(|(name, _, _)| *name).collect();
        for name in top_level_names() {
            assert!(
                groups.contains(&name),
                "顶层命令 {name} 没进 GROUPS,`cgw help` 的树里会看不到它"
            );
        }
        for (name, _, _) in GROUPS {
            assert!(
                !children_of(name).is_empty() || find_command(name).is_some(),
                "GROUPS 里的 {name} 既不是命令也没有子命令"
            );
        }
    }

    /// The WebUI table must really point at UI locations: no key entry may be left blank.
    #[test]
    fn key_commands_say_where_they_live_in_the_webui() {
        let must_have = [
            "clients.add",
            "clients.edit",
            "clients.test",
            "basedir",
            "env",
            "projects.add",
            "tree",
            "cat",
            "tasks.save",
            "batch.save",
            "batch.run",
            "push",
            "pull",
            "export",
            "exports",
            "jobs.cancel",
            "jobs.retry",
            "manifests.save",
            "manifests.detect",
            "manifests.refresh",
            "manifests.status",
            "manifests.import",
            "manifests.export",
            "set.port",
            "set.listen",
            "set.user",
            "set.roots",
            "set.mcp",
            "logs.client",
            "run",
        ];
        for name in must_have {
            let entry = find_command(name).unwrap_or_else(|| panic!("命令表里没有 {name}"));
            assert!(
                !entry.webui.is_empty(),
                "{name} 在界面上的位置没写(WebUI 能做的话,这里必须指出来)"
            );
            assert!(
                !entry.webui_en.is_empty(),
                "{name} WebUI English description missing"
            );
        }
    }

    /// Path resolution: `cgw tasks -h` and `cgw help tasks` must land in the same place.
    #[test]
    fn help_path_accepts_both_spellings() {
        let words = |items: &[&str]| items.iter().map(|item| item.to_string()).collect::<Vec<_>>();
        assert_eq!(help_path(&words(&["help", "tasks"])), "tasks");
        assert_eq!(help_path(&words(&["tasks"])), "tasks");
        assert_eq!(help_path(&words(&["help", "tasks", "save"])), "tasks.save");
        assert_eq!(help_path(&words(&["tasks", "save"])), "tasks.save");
        assert_eq!(help_path(&words(&["tasks", "-h"])), "tasks");
        assert_eq!(help_path(&words(&["help", "--webui"])), "");
        assert_eq!(help_path(&words(&[])), "");
    }

    /// `help --json` is the machine-readable copy, so its fields must be complete.
    #[test]
    fn help_json_is_complete_and_parseable() {
        for lang in [Lang::ZhHans, Lang::EnUS] {
            let value: serde_json::Value = serde_json::from_str(&json_for(lang)).expect("--json must be valid JSON");
            let commands = value["commands"].as_array().expect("commands");
            assert_eq!(commands.len(), HELP.len());
            for entry in commands {
                for field in ["name", "usage", "summary", "args", "webui"] {
                    assert!(entry.get(field).is_some(), "Command missing {field}: {entry}");
                }
            }
            assert_eq!(value["exitCodes"].as_object().map(|m| m.len()), Some(8));
            assert!(value["globalFlags"].as_array().is_some_and(|f| f.len() >= 4));
            let parity = value["webuiParity"].as_array().expect("webuiParity");
            assert_eq!(
                parity.len(),
                HELP.iter().filter(|entry| !entry.webui_for(lang).is_empty()).count()
            );
        }
    }

    /// English JSON contains no CJK characters (guaranteeing MCP purity).
    #[test]
    fn english_json_is_pure_ascii_and_no_cjk() {
        let text = json_for(Lang::EnUS);
        for c in text.chars() {
            let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c);
            assert!(!is_cjk, "English help JSON contains CJK character: {c}");
        }
    }

    /// All three views (tree / drill-down / table) must render from one table and be non-empty.
    #[test]
    fn every_view_renders_something() {
        assert!(tree_for(Lang::ZhHans).contains("用法: cgw"));
        assert!(tree_for(Lang::EnUS).contains("Usage: cgw"));
        let save = find_command("tasks.save").unwrap();
        assert!(command_for(save, Lang::ZhHans).contains("用法: cgw tasks save"));
        assert!(command_for(save, Lang::EnUS).contains("Usage: cgw tasks save"));
        let kids = children_of("tasks");
        let rendered_zh = group_for("tasks", &kids, Lang::ZhHans);
        assert!(rendered_zh.starts_with("cgw tasks —— "), "{rendered_zh}");
        assert!(rendered_zh.contains("save"), "{rendered_zh}");
        assert!(rendered_zh.contains("项目任务"), "{rendered_zh}");

        let rendered_en = group_for("tasks", &kids, Lang::EnUS);
        assert!(rendered_en.starts_with("cgw tasks — "), "{rendered_en}");
        assert!(rendered_en.contains("save"), "{rendered_en}");
        assert!(rendered_en.contains("Project tasks"), "{rendered_en}");

        assert!(webui_table_for(Lang::ZhHans).contains("WebUI 上的位置"));
        assert!(webui_table_for(Lang::EnUS).contains("WebUI Location"));

        match resolve_for(&["help".to_string(), "tasks".to_string()], Lang::ZhHans) {
            Resolved::Group(text) => assert!(text.contains("save")),
            _ => panic!("cgw help tasks should be a group"),
        }
        match resolve_for(&["nope".to_string()], Lang::ZhHans) {
            Resolved::Unknown { hints, .. } => assert!(hints.contains(&"tasks".to_string())),
            _ => panic!("Unknown command should provide hints"),
        }
    }
}
