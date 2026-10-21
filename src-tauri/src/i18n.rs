//! Language resolution and every user-visible string.
//!
//! `language` in the config file: `auto` / `zh-Hans` / `en-US`; `auto` follows the OS
//! locale (Chinese -> zh-Hans, everything else -> en-US).
//!
//! Log text lives here; window chrome lives in `web/main.js`'s `CHROME` table, keyed by `lang`.

use std::path::Path;

pub const AUTO: &str = "auto";
pub const ZH_HANS: &str = "zh-Hans";
pub const EN_US: &str = "en-US";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    ZhHans,
    EnUS,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhHans => ZH_HANS,
            Lang::EnUS => EN_US,
        }
    }

    /// Infer the language from the system locale.
    pub fn from_system() -> Lang {
        match sys_locale::get_locale() {
            Some(locale) if locale.to_ascii_lowercase().starts_with("zh") => Lang::ZhHans,
            _ => Lang::EnUS,
        }
    }

    /// Resolve the config value, returning (language, source note for the CLI).
    /// An invalid value does not error; it falls back to `auto`.
    pub fn resolve(setting: &str) -> (Lang, String) {
        let trimmed = setting.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(AUTO) {
            let sys = sys_locale::get_locale().unwrap_or_else(|| "unknown".to_string());
            (Lang::from_system(), format!("auto ({sys})"))
        } else if trimmed.eq_ignore_ascii_case(ZH_HANS) || trimmed.eq_ignore_ascii_case("zh") {
            (Lang::ZhHans, format!("{ZH_HANS} (config)"))
        } else if trimmed.eq_ignore_ascii_case(EN_US) || trimmed.eq_ignore_ascii_case("en") {
            (Lang::EnUS, format!("{EN_US} (config)"))
        } else {
            let sys = sys_locale::get_locale().unwrap_or_else(|| "unknown".to_string());
            (
                Lang::from_system(),
                format!("auto ({sys}) - unknown value \"{trimmed}\", expected auto/{ZH_HANS}/{EN_US}"),
            )
        }
    }
}

/// Whether the `language` value is recognized (drives the CLI warning).
pub fn is_known_language(setting: &str) -> bool {
    let s = setting.trim();
    s.is_empty()
        || s.eq_ignore_ascii_case(AUTO)
        || s.eq_ignore_ascii_case(ZH_HANS)
        || s.eq_ignore_ascii_case("zh")
        || s.eq_ignore_ascii_case(EN_US)
        || s.eq_ignore_ascii_case("en")
}

// --- The process-wide current language --------------------------------------
//
// Language is **ambient configuration**, not a per-call parameter: one process has exactly
// one `language` setting (the daemon's config, or the CLI's `--url` target's status). Deep
// modules (`paths`, `ssh`, `fileops`, `workflow`, ...) sit many calls below the handler that
// knows the language, so threading a `Lang` through every intermediate signature would touch
// hundreds of call sites for no benefit. Instead each process publishes its resolved language
// once, and message builders read it from here -- the same shape as `gettext` / a locale.
//
// Anything that already holds a `Lang` (server handlers, CLI commands) should keep passing it
// explicitly to the `text()` / `err_*()` / `*_summary()` helpers; `current()` is the fallback
// for the deep layers that cannot see one.

/// Sentinel meaning "not set yet".
const UNSET: u8 = 0xff;

static CURRENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNSET);
static FALLBACK: std::sync::OnceLock<Lang> = std::sync::OnceLock::new();

/// Publish the language for this process. Called once at startup (and again whenever the
/// configured language changes).
pub fn set_current(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::ZhHans => 0,
            Lang::EnUS => 1,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The language messages should be built in.
///
/// Falls back to the system locale until [`set_current`] runs, so even strings emitted
/// during startup (before the config is read) come out in a sensible language.
pub fn current() -> Lang {
    match CURRENT.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Lang::ZhHans,
        1 => Lang::EnUS,
        _ => *FALLBACK.get_or_init(Lang::from_system),
    }
}

/// Every static string for one language.
pub struct Texts {
    pub app_name: &'static str,
    // Startup info
    pub label_config: &'static str,
    pub label_language: &'static str,
    pub label_port: &'static str,
    pub label_listen: &'static str,
    pub label_logs: &'static str,
    pub unit_days: &'static str,
    /// Build-type directory
    pub label_manifests: &'static str,
    /// Cross-project workflow template directory (design doc §14.3).
    pub label_workflows: &'static str,
    /// Where the env-var shim lands (CGW_HOME).
    pub label_cgw_home: &'static str,
    pub manifests_seeded: &'static str,
    pub manifests_migrated: &'static str,
    pub unit_files: &'static str,
    pub listen_lan: &'static str,
    pub listen_wan: &'static str,
    pub label_auth: &'static str,
    pub auth_none: &'static str,
    pub warn_wan_no_auth: &'static str,
    pub config_updated: &'static str,
    // Clients
    pub client_saved: &'static str,
    pub client_deleted: &'static str,
    pub client_tested: &'static str,
    pub dialog_pick_key: &'static str,
    // Base directory and projects
    pub dialog_pick_folder: &'static str,
    pub base_dir_updated: &'static str,
    pub project_added: &'static str,
    pub project_deleted: &'static str,
    pub project_dir_ready: &'static str,
    pub project_dir_failed: &'static str,
    pub sync_ok: &'static str,
    pub sync_failed: &'static str,
    pub build_started: &'static str,
    pub build_finished: &'static str,
    pub manifest_invalid: &'static str,
    /// The task input contract failed (undeclared / bad value / path escapes after expansion).
    pub input_invalid: &'static str,
    /// An output was declared but not found (or of the wrong kind) after the run.
    pub artifacts_missing: &'static str,
    pub batch_started: &'static str,
    pub batch_finished: &'static str,
    pub batch_failed: &'static str,
    pub batch_cancelled: &'static str,
    pub host_key_forgotten: &'static str,
    pub host_key_trusted: &'static str,
    pub env_started: &'static str,
    pub env_finished: &'static str,
    pub preset_restored: &'static str,
    /// A workflow template was saved (design doc §14.3).
    pub workflow_saved: &'static str,
    /// A workflow template was applied to a project.
    pub workflow_applied: &'static str,
    pub workflow_restored: &'static str,
    pub manifest_saved: &'static str,
    pub manifest_deleted: &'static str,
    pub project_busy: &'static str,
    /// Host-level mutex: **another project** is running on the same client (design doc §7.6.6).
    pub host_busy: &'static str,
    pub auth_local_only: &'static str,
    pub lbl_base_dir: &'static str,
    pub project_remote_label: &'static str,
    // Client labels and hints
    pub clients_count: &'static str,
    pub lbl_name: &'static str,
    pub lbl_host: &'static str,
    pub lbl_port: &'static str,
    pub lbl_auth: &'static str,
    pub lbl_tags: &'static str,
    pub auth_password: &'static str,
    pub auth_key: &'static str,
    pub host_key: &'static str,
    pub host_key_none: &'static str,
    pub testing: &'static str,
    pub cmd_api: &'static str,
    // set (windowed version of the settings page)
    pub cmd_set: &'static str,
    pub set_help: &'static str,
    pub set_copy: &'static str,
    pub set_overview_title: &'static str,
    pub set_overview_hint: &'static str,
    pub set_saved: &'static str,
    pub set_saved_restart: &'static str,
    pub set_failed: &'static str,
    pub set_roots_title: &'static str,
    pub set_roots_default: &'static str,
    pub set_roots_hint: &'static str,
    pub set_password_set: &'static str,
    pub set_password_none: &'static str,
    pub set_group_ports: &'static str,
    pub set_group_listen: &'static str,
    pub set_group_user: &'static str,
    pub set_group_ui: &'static str,
    pub set_group_roots: &'static str,
    pub set_group_mcp: &'static str,
    pub set_usage_port: &'static str,
    pub set_usage_listen: &'static str,
    pub set_usage_user: &'static str,
    pub set_usage_language: &'static str,
    pub set_usage_login_interval: &'static str,
    pub set_usage_log_retain: &'static str,
    pub set_usage_roots: &'static str,
    pub set_usage_mcp: &'static str,
    // help <topic>
    pub help_topics_hint: &'static str,
    pub help_no_topic: &'static str,
    pub doc_help: &'static str,
    pub doc_status: &'static str,
    pub doc_api: &'static str,
    pub doc_more: &'static str,
    pub doc_diag: &'static str,
    pub doc_webui: &'static str,
    pub doc_clear: &'static str,
    pub doc_exit: &'static str,
    pub api_usage: &'static str,
    pub api_unavailable: &'static str,
    pub running_jobs: &'static str,
    pub cmd_diag: &'static str,
    pub cmd_test: &'static str,
    pub cmd_instance_status: &'static str,
    pub cmd_close_window: &'static str,
    pub diag_no_clients: &'static str,
    pub diag_pick: &'static str,
    pub diag_not_found: &'static str,
    pub diag_ambiguous: &'static str,
    pub diag_opened: &'static str,
    pub diag_vanished: &'static str,
    pub label_webui: &'static str,
    pub port_source_config: &'static str,
    pub port_source_auto: &'static str,
    pub port_auto_note: &'static str,
    pub config_created: &'static str,
    pub config_unknown_language: &'static str,
    pub webui_started: &'static str,
    // MCP adapter (design doc §12 Phase 6)
    pub label_mcp: &'static str,
    pub mcp_disabled: &'static str,
    pub mcp_failed: &'static str,
    pub warn_mcp_wan_no_auth: &'static str,
    pub browser_opened: &'static str,
    pub browser_failed: &'static str,
    pub fatal_hint: &'static str,
    pub input_hint: &'static str,
    pub bye: &'static str,
    // help
    pub help_header: &'static str,
    pub cmd_help: &'static str,
    pub cmd_status: &'static str,
    pub cmd_webui: &'static str,
    pub cmd_clear: &'static str,
    pub cmd_exit: &'static str,
    pub unknown_hint: &'static str,
    pub webui_unavailable: &'static str,
    // WebUI page (GUI)
    pub hello: &'static str,
    pub page_note: &'static str,
}

pub fn text(lang: Lang) -> Texts {
    match lang {
        Lang::ZhHans => Texts {
            app_name: "CompileGateway",
            label_config: "配置文件",
            label_language: "语言",
            label_port: "监听端口",
            label_listen: "监听模式",
            label_manifests: "构建类型目录",
            label_workflows: "工作流模板目录",
            label_cgw_home: "cgw 共享目录",
            manifests_seeded: "首次运行:已生成构建类型模板",
            manifests_migrated: "构建类型目录已改名",
            unit_files: "份",
            label_logs: "日志目录",
            unit_days: "天",
            listen_lan: "lan - 仅本机访问 (localhost / 127.0.0.1)",
            listen_wan: "wan - 允许其他设备访问 (本机IP:端口)",
            label_auth: "访问凭据",
            auth_none: "未设置(lan 下无影响,wan 下建议设置)",
            warn_wan_no_auth: "当前是 wan 模式(其他设备可以用 本机IP:端口 访问),但没有设置用户名和密码:\
任何能连到这个端口的人都能打开 WebUI 并修改配置。",
            config_updated: "配置已从 WebUI 更新",
            client_saved: "客户机已保存",
            client_deleted: "客户机已删除",
            client_tested: "客户机连接测试",
            dialog_pick_key: "选择 SSH 私钥文件",
            dialog_pick_folder: "选择要导入的项目文件夹",
            base_dir_updated: "基准目录已更新",
            project_added: "项目已登记",
            project_deleted: "项目已移除",
            project_dir_ready: "远端目录已就绪",
            project_dir_failed: "远端目录创建失败:",
            sync_ok: "同步完成",
            sync_failed: "同步失败",
            build_started: "开始执行",
            build_finished: "执行结束",
            manifest_invalid: "构建类型不可用",
            input_invalid: "任务的输入不合法",
            artifacts_missing: "声明的产物没找到",
            batch_started: "批量工作流开始",
            batch_finished: "批量工作流完成",
            batch_failed: "批量工作流失败",
            batch_cancelled: "批量工作流已取消",
            host_key_forgotten: "已清除记录的主机指纹(下次连接重新按首次信任处理)",
            host_key_trusted: "已信任新的主机指纹",
            env_started: "开始检测目标机环境",
            env_finished: "环境检测结束",
            preset_restored: "已重置缺失预设",
            workflow_saved: "已保存工作流模板",
            workflow_applied: "已把工作流模板应用到项目",
            workflow_restored: "已补齐缺失的工作流模板",
            manifest_saved: "构建类型已保存",
            manifest_deleted: "构建类型已删除",
            project_busy: "这个项目正在执行 {kind} 任务(job {job}),同一项目不允许并发,请稍后重试。",
            host_busy: "同一台客户机上,项目 {project} 正在执行 {kind} 任务(job {job})——\
                        这台机器一次只跑一个(hostLock 已打开)。换一台客户机,或者用 enqueue 排队等它结束。",
            auth_local_only: "访问凭据只允许在本机(127.0.0.1)修改。",
            lbl_base_dir: "基准目录",
            project_remote_label: "解析后路径",
            clients_count: "客户机数量",
            lbl_name: "名称",
            lbl_host: "IP 地址",
            lbl_port: "端口",
            lbl_auth: "验证方式",
            lbl_tags: "标签",
            auth_password: "登录信息",
            auth_key: "密钥文件",
            host_key: "主机指纹",
            host_key_none: "尚未记录(首次连接成功后写入)",
            testing: "正在测试连接",
            cmd_set: "改配置:端口 / 监听 / 凭据 / 语言 / 限流 / 日志 / 宿主目录 / MCP",
            set_help: "改配置。**配置页上能改的,这里都能改**,分类和界面一一对应。
  set                          列出全部设置和当前值(同 set all)
  set port   --webui 65251     改 WebUI 端口(要重启程序才生效)
  set port   --mcp 3994        改 MCP 端口(立即生效;两个端口互相独立)
  set listen --webui lan|wan   只监听本机 / 允许其他设备访问
  set user   --name U --password P   改访问凭据(只有本机可改);--clear 清掉
  set language zh-Hans|en-US|auto
  set login-interval 0         登录限流间隔秒数,0 = 不限流
  set log-retain 7             日志保留天数
  set roots [list]             宿主目录白名单(browse / 登记项目 / 探测共用)
  set roots add F:\\CODEX       加一个;set roots rm <路径> 去掉
  set mcp   on|off             开关 MCP 适配层(立即生效)
  set all --json               当前配置的原始 JSON
每一项都等价于 WebUI 配置页上的对应控件。",
            set_copy: "任何命令都能在独立 CLI(`cgw ...`,放进了 PATH)里用同一套写法敲。",
            set_overview_title: "当前设置(括号里是改法):",
            set_overview_hint: "每一项都能在 WebUI 配置页上点到;`set all --json` 给程序看。",
            set_saved: "已保存(立即生效)。",
            set_saved_restart: "已保存。端口 / 监听模式的改动要重启程序才生效(MCP 与宿主目录立即生效)。",
            set_failed: "保存失败",
            set_roots_title: "当前允许的宿主目录(按顺序):",
            set_roots_default: "   (用户主目录,默认就有)",
            set_roots_hint: "加一个:`set roots add <绝对路径>`;去掉:`set roots rm <绝对路径>`",
            set_password_set: "已设置",
            set_password_none: "未设置",
            set_group_ports: "端口",
            set_group_listen: "监听范围",
            set_group_user: "访问凭据",
            set_group_ui: "界面",
            set_group_roots: "宿主目录",
            set_group_mcp: "MCP",
            set_usage_port: "set port --webui N | --mcp N",
            set_usage_listen: "set listen --webui lan|wan | --mcp lan|wan",
            set_usage_user: "set user --name U [--password P] | --clear",
            set_usage_language: "set language auto|zh-Hans|en-US",
            set_usage_login_interval: "set login-interval N",
            set_usage_log_retain: "set log-retain N",
            set_usage_roots: "set roots [list] | add|rm <路径>",
            set_usage_mcp: "set mcp on|off",
            help_topics_hint: "看某一类的说明:`help --<类>`(例如 `help --set`),等价于裸敲那一类。",
            help_no_topic: "没有这一类:{}。",
            doc_help: "`help` 列出所有命令;`help --<类>` 看某一类的说明(等价于裸敲那一类);`help --set` 看怎么改配置。",
            doc_status: "显示端口、监听模式、语言、配置文件和 WebUI 地址,以及客户机与正在跑的 job 数量。数据来自 daemon 的 /api/status(服务没起来时退回进程内读取)。",
            doc_api: "直接打 daemon 的 HTTP API,是「WebUI 能做的这里也能做」的兜底。
  api GET /api/clients                看客户机列表
  api POST /api/manifests/reload {}   刷新构建类型目录
  api GET /api/projects               看项目
方法和路径之间用空格;第三段是 JSON 请求体(可省略)。响应会格式化后打到日志区。",
            cmd_api: "直接调 daemon 的 HTTP API(api GET /api/clients)",
            doc_more: "其余命令(clients / projects / tasks / run / push / pull / export / jobs / manifests / logs / schema / mcp ...)由**和独立 CLI 完全同一份实现**提供,敲 `help <命令>` 看用法。",
            doc_diag: "拉起某台客户机的独立诊断窗口:`diag` 列出所有客户机,`diag <名称>` 开一个。窗口里可以 test / status,exit 关掉它。",
            doc_webui: "用系统默认浏览器打开 WebUI 图形界面(同 open)。",
            doc_clear: "清空当前窗口的日志区(同 cls);不影响落盘日志。",
            doc_exit: "退出程序(同 quit);实例窗口里则是关闭那一扇窗。",
            api_usage: "用法:api <METHOD> <PATH> [JSON],例如 api GET /api/clients",
            api_unavailable: "daemon 的 HTTP 服务没在跑,这条命令只能读进程内状态。",
            running_jobs: "执行中的任务",
            cmd_diag: "拉起某台客户机的诊断窗口(diag 名称)",
            cmd_test: "测试这台客户机的 SSH 连接",
            cmd_instance_status: "显示这台客户机的连接信息",
            cmd_close_window: "关闭本窗口",
            diag_no_clients: "还没有配置任何客户机,请先到 WebUI 的「客户机」页添加。",
            diag_pick: "用法:diag <名称>。当前可用:",
            diag_not_found: "找不到客户机:{}",
            diag_ambiguous: "「{}」匹配到多台客户机,请写全名:",
            diag_opened: "已打开客户机窗口",
            diag_vanished: "这台客户机已经从列表里删除了,请关闭本窗口。",
            label_webui: "WebUI 图形界面",
            port_source_config: "来自配置文件",
            port_source_auto: "自动挑选(配置文件未指定)",
            port_auto_note: "如需固定端口,请编辑上方配置文件里的 \"port\"",
            config_created: "未找到配置文件,已生成默认配置",
            config_unknown_language: "配置文件里的 language 取值无法识别",
            webui_started: "WebUI 服务已启动",
            label_mcp: "MCP(AI agent)",
            mcp_disabled: "MCP 适配层未启用(配置里的 mcp.enabled = false)",
            mcp_failed: "MCP 适配层没起来",
            warn_mcp_wan_no_auth: "MCP 监听在 wan 模式(其他设备可以访问)但没有设置用户名和密码:\
请设置访问凭据,或者把 mcp.listen 改回 lan。",
            browser_opened: "已用默认浏览器打开 WebUI 图形界面。",
            browser_failed: "无法自动打开浏览器,请手动访问上面的地址。",
            fatal_hint: "本程序不会重试。处理完之后请关闭本窗口并重新打开。",
            input_hint: "输入 help 查看可用命令。",
            bye: "再见。",
            help_header: "可用命令:",
            cmd_help: "显示这份帮助",
            cmd_status: "显示配置、语言、端口和 WebUI 地址",
            cmd_webui: "在浏览器里打开 WebUI 图形界面",
            cmd_clear: "清空日志",
            cmd_exit: "退出程序(同 quit)",
            unknown_hint: "输入 help 查看可用命令。",
            webui_unavailable: "WebUI 未运行(启动时端口未能绑定)。",
            hello: "运行状态概览",
            page_note: "CompileGateway WebUI 图形界面",
        },
        Lang::EnUS => Texts {
            app_name: "CompileGateway",
            label_config: "Config file",
            label_language: "Language",
            label_port: "Port",
            label_listen: "Listen",
            label_manifests: "Manifests",
            label_workflows: "Workflow presets",
            label_cgw_home: "cgw shared dir",
            manifests_seeded: "First run: build-type templates written",
            manifests_migrated: "Build-type directory renamed",
            unit_files: "files",
            label_logs: "Log directory",
            unit_days: "days",
            listen_lan: "lan - this machine only (localhost / 127.0.0.1)",
            listen_wan: "wan - reachable by other devices (host-ip:port)",
            label_auth: "Credentials",
            auth_none: "not set (fine for lan, recommended for wan)",
            warn_wan_no_auth: "Listen mode is wan (reachable by other devices) but no username/password is set: \
anyone who can reach this port can open the WebUI and change the config.",
            config_updated: "Config updated from the WebUI",
            client_saved: "Client saved",
            client_deleted: "Client deleted",
            client_tested: "Client connection test",
            dialog_pick_key: "Select SSH private key",
            dialog_pick_folder: "Select the project folder to import",
            base_dir_updated: "Base directory updated",
            project_added: "Project registered",
            project_deleted: "Project removed",
            project_dir_ready: "remote directory is ready",
            project_dir_failed: "could not create the remote directory:",
            sync_ok: "Sync finished",
            sync_failed: "Sync failed",
            build_started: "Started",
            build_finished: "Finished",
            manifest_invalid: "Build type unavailable",
            input_invalid: "Task inputs are invalid",
            artifacts_missing: "Declared artifacts were not produced",
            preset_restored: "Restored missing presets",
            workflow_saved: "Saved workflow preset",
            workflow_applied: "Applied workflow preset to project",
            workflow_restored: "Restored missing workflow presets",
            manifest_saved: "Build type saved",
            manifest_deleted: "Build type deleted",
            batch_started: "Batch started",
            batch_finished: "Batch finished",
            batch_failed: "Batch failed",
            batch_cancelled: "Batch cancelled",
            host_key_forgotten: "Forgot the stored host key fingerprint (the next connection is trusted as first-time)",
            host_key_trusted: "Trusted the new host key fingerprint",
            env_started: "Checking the target machine's environment",
            env_finished: "Environment check finished",
            project_busy: "This project is already running a {kind} job (job {job}); concurrent runs on one project are not allowed.",
            host_busy: "Project {project} is already running a {kind} job (job {job}) on the same client — this host runs one job at a time (hostLock is on). Use another client, or enqueue to wait for it.",
            auth_local_only: "Credentials can only be changed from this machine (127.0.0.1).",
            lbl_base_dir: "Base directory",
            project_remote_label: "Resolved path",
            clients_count: "Clients",
            lbl_name: "Name",
            lbl_host: "IP address",
            lbl_port: "Port",
            lbl_auth: "Auth",
            lbl_tags: "Tags",
            auth_password: "Password",
            auth_key: "Key file",
            host_key: "Host key",
            host_key_none: "not recorded yet (written after the first successful connect)",
            testing: "testing connection",
            cmd_set: "Change settings: ports / listen / credentials / language / rate limit / logs / host dirs / MCP",
            set_help: "Change settings. **Everything the settings page can change is changeable here**, grouped the same way.
  set                          list every setting with its current value (same as set all)
  set port   --webui 65251     WebUI port (takes effect after a restart)
  set port   --mcp 3994        MCP port (applies immediately; the two ports are independent)
  set listen --webui lan|wan   loopback only / reachable by other devices
  set user   --name U --password P   credentials (loopback only); --clear removes them
  set language zh-Hans|en-US|auto
  set login-interval 0         minimum seconds between login attempts (0 = unlimited)
  set log-retain 7             days of logs to keep
  set roots [list]             allowed host directories (browse / register / detect share them)
  set roots add F:\\CODEX       add one; set roots rm <path> removes it
  set mcp   on|off             toggle the MCP adapter (applies immediately)
  set all --json               raw JSON of the current config
Every item matches a control on the WebUI settings page.",
            set_copy: "Every command also works, with identical syntax, in the standalone `cgw` (the one you can put on PATH).",
            set_overview_title: "Current settings (how to change each is in parentheses):",
            set_overview_hint: "Every item is also on the WebUI settings page; `set all --json` is for scripts.",
            set_saved: "Saved (applies immediately).",
            set_saved_restart: "Saved. Port and listen-mode changes apply after a restart (MCP and host dirs apply now).",
            set_failed: "save failed",
            set_roots_title: "Allowed host directories (in order):",
            set_roots_default: "   (the user home directory, always allowed)",
            set_roots_hint: "add: `set roots add <absolute path>`; remove: `set roots rm <absolute path>`",
            set_password_set: "set",
            set_password_none: "not set",
            set_group_ports: "Ports",
            set_group_listen: "Listen mode",
            set_group_user: "Credentials",
            set_group_ui: "Interface",
            set_group_roots: "Host dirs",
            set_group_mcp: "MCP",
            set_usage_port: "set port --webui N | --mcp N",
            set_usage_listen: "set listen --webui lan|wan | --mcp lan|wan",
            set_usage_user: "set user --name U [--password P] | --clear",
            set_usage_language: "set language auto|zh-Hans|en-US",
            set_usage_login_interval: "set login-interval N",
            set_usage_log_retain: "set log-retain N",
            set_usage_roots: "set roots [list] | add|rm <path>",
            set_usage_mcp: "set mcp on|off",
            help_topics_hint: "For one topic: `help --<topic>` (e.g. `help --set`) - same as typing that command bare.",
            help_no_topic: "No such topic: {}.",
            doc_help: "`help` lists every command; `help --<topic>` explains one topic (same as typing it bare); `help --set` shows how to change settings.",
            doc_status: "Shows port, listen mode, language, config file and WebUI URL, plus how many clients and jobs are running. Comes from the daemon's /api/status.",
            doc_api: "Call the daemon HTTP API directly - the escape hatch behind \"whatever the WebUI can do, this can do\".
  api GET /api/clients                list clients
  api POST /api/manifests/reload {}   re-read the build-type directory
  api GET /api/projects               list projects
Method and path are separated by a space; an optional third part is a JSON body.",
            doc_more: "All other commands (clients / projects / tasks / run / push / pull / export / jobs / manifests / logs / schema / mcp ...) come from **the very same implementation as the standalone CLI**; type `help <command>` for usage.",
            doc_diag: "Open a client's own diagnostic window: `diag` lists the clients, `diag <name>` opens one. Inside it you can run test / status; exit closes it.",
            doc_webui: "Open the WebUI in your default browser (same as open).",
            doc_clear: "Clear this window's log area (same as cls); on-disk logs are untouched.",
            doc_exit: "Quit the app (same as quit); in a client window it closes just that window.",
            cmd_api: "Call the daemon HTTP API directly (api GET /api/clients)",
            api_usage: "usage: api <METHOD> <PATH> [JSON], e.g. api GET /api/clients",
            api_unavailable: "The daemon HTTP server is not running; showing in-process state only.",
            running_jobs: "Jobs running",
            cmd_diag: "Open the diagnostic window of a client (diag <name>)",
            cmd_test: "Test the SSH connection of this client",
            cmd_instance_status: "Show this client's connection info",
            cmd_close_window: "Close this window",
            diag_no_clients: "No clients configured yet - add one on the WebUI Clients page.",
            diag_pick: "Usage: diag <name>. Available:",
            diag_not_found: "No such client: {}",
            diag_ambiguous: "\"{}\" matches several clients, please be more specific:",
            diag_opened: "Client window opened",
            diag_vanished: "This client was removed from the list; close this window.",
            label_webui: "WebUI",
            port_source_config: "from config file",
            port_source_auto: "auto-selected (not set in config)",
            port_auto_note: "Edit \"port\" in the config file above to pin it",
            config_created: "No config file found, a default one was created",
            config_unknown_language: "Unrecognized \"language\" value in config",
            webui_started: "WebUI server started",
            label_mcp: "MCP (AI agent)",
            mcp_disabled: "MCP adapter is off (mcp.enabled = false in the config)",
            mcp_failed: "MCP adapter failed to start",
            warn_mcp_wan_no_auth: "MCP listens in wan mode (reachable by other devices) but no \
username/password is set: set credentials, or switch mcp.listen back to lan.",
            browser_opened: "Opened the WebUI in your default browser.",
            browser_failed: "Could not open a browser automatically, visit the URL above.",
            fatal_hint: "This app will not retry. Close this window and start it again once fixed.",
            input_hint: "Type `help` for the list of commands.",
            bye: "Bye.",
            help_header: "Commands:",
            cmd_help: "Show this help",
            cmd_status: "Show config, language, port and WebUI URL",
            cmd_webui: "Open the WebUI in your default browser",
            cmd_clear: "Clear the log",
            cmd_exit: "Quit the app (same as quit)",
            unknown_hint: "Type `help` for the list of commands.",
            webui_unavailable: "WebUI is not running (the port could not be bound at startup).",
            hello: "System Status",
            page_note: "CompileGateway WebUI",
        },
    }
}

// --- Error messages (parameterized, assembled per language) -----------------

pub fn err_port_in_use(lang: Lang, port: u16) -> String {
    match lang {
        Lang::ZhHans => format!(
            "配置文件里指定的端口 {port} 已被其他程序占用。\n\
             \x20       请关闭占用该端口的程序,然后重新打开本软件。\n\
             \x20       查看占用进程:netstat -ano | findstr :{port} (Windows) / lsof -i :{port} (macOS, Linux)"
        ),
        Lang::EnUS => format!(
            "Port {port} from the config file is already in use by another program.\n\
             \x20       Close the program holding that port, then start this app again.\n\
             \x20       Find the owner: netstat -ano | findstr :{port} (Windows) / lsof -i :{port} (macOS, Linux)"
        ),
    }
}

pub fn err_bind_failed(lang: Lang, addr: &str, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!(
            "端口 {addr} 绑定失败:{err}\n\
             \x20       请检查该端口是否被保留或需要特权,或在配置文件里换一个端口。"
        ),
        Lang::EnUS => format!(
            "Failed to bind {addr}: {err}\n\
             \x20       The port may be reserved or privileged; try another one in the config file."
        ),
    }
}

pub fn err_config_read(lang: Lang, path: &Path, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("读取配置文件失败:{} - {err}", path.display()),
        Lang::EnUS => format!("Failed to read config file: {} - {err}", path.display()),
    }
}

pub fn err_config_parse(lang: Lang, path: &Path, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!(
            "配置文件不是合法 JSON:{} - {err}\n\
             \x20       请修正该文件后重新打开本软件。",
            path.display()
        ),
        Lang::EnUS => format!(
            "Config file is not valid JSON: {} - {err}\n\
             \x20       Fix the file and start the app again.",
            path.display()
        ),
    }
}

pub fn err_config_write(lang: Lang, path: &Path, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("写入默认配置文件失败:{} - {err}", path.display()),
        Lang::EnUS => format!("Failed to write default config file: {} - {err}", path.display()),
    }
}

pub fn err_no_config_dir(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "无法定位程序所在目录,无法确定配置文件位置。".to_string(),
        Lang::EnUS => "Could not locate the executable directory for the config file.".to_string(),
    }
}

// --- Snapshot / archive one-liners (user-visible summaries) -----------------

/// One-line summary of a manifest-directory refresh (`cgw manifests refresh`).
///
/// The counts live in the response as structured `added` / `changed` / `removed` arrays;
/// this string is only the human sentence, so it must follow the configured language.
pub fn manifest_diff_summary(
    lang: Lang,
    added: &[String],
    changed: &[String],
    removed: &[String],
) -> String {
    if added.is_empty() && changed.is_empty() && removed.is_empty() {
        return match lang {
            Lang::ZhHans => "没有变化".to_string(),
            Lang::EnUS => "no changes".to_string(),
        };
    }
    let mut parts: Vec<String> = Vec::new();
    if !added.is_empty() {
        parts.push(match lang {
            Lang::ZhHans => format!("新增 {}", added.join(", ")),
            Lang::EnUS => format!("added {}", added.join(", ")),
        });
    }
    if !changed.is_empty() {
        parts.push(match lang {
            Lang::ZhHans => format!("内容变了 {}", changed.join(", ")),
            Lang::EnUS => format!("changed {}", changed.join(", ")),
        });
    }
    if !removed.is_empty() {
        parts.push(match lang {
            Lang::ZhHans => format!("已删除 {}", removed.join(", ")),
            Lang::EnUS => format!("removed {}", removed.join(", ")),
        });
    }
    parts.join(";")
}

/// Log line written when the build-type directory is re-read.
pub fn manifest_refreshed_log(lang: Lang, summary: &str, total: usize, invalid: usize) -> String {
    match lang {
        Lang::ZhHans => format!("构建类型已刷新:{summary} (共 {total} 份,{invalid} 份不合法)"),
        Lang::EnUS => {
            format!("build types refreshed: {summary} ({total} total, {invalid} invalid)")
        }
    }
}

/// One-line summary of what went into a push archive.
pub fn archive_stats_summary(
    lang: Lang,
    files: u64,
    bytes: &str,
    links: u64,
    outside_links: u64,
    crlf_normalized: u64,
) -> String {
    let mut text = match lang {
        Lang::ZhHans => format!("{files} 个文件 / {bytes}"),
        Lang::EnUS => format!("{files} files / {bytes}"),
    };
    if links > 0 {
        match lang {
            Lang::ZhHans => {
                text.push_str(&format!("(含 {links} 个符号链接"));
                if outside_links > 0 {
                    text.push_str(&format!(",其中 {outside_links} 个指向项目之外"));
                }
            }
            Lang::EnUS => {
                text.push_str(&format!(" ({links} symlinks"));
                if outside_links > 0 {
                    text.push_str(&format!(", {outside_links} pointing outside the project"));
                }
            }
        }
        text.push(')');
    }
    if crlf_normalized > 0 {
        match lang {
            Lang::ZhHans => {
                text.push_str(&format!(";{crlf_normalized} 个脚本的换行已从 CRLF 归一化成 LF"));
            }
            Lang::EnUS => {
                text.push_str(&format!(
                    "; line endings of {crlf_normalized} scripts normalized from CRLF to LF"
                ));
            }
        }
    }
    text
}

// --- Sync (push / pull) messages --------------------------------------------
//
// These end up as job messages in the WebUI / CLI, so they follow the configured language.

pub fn sync_source_missing(lang: Lang, path: &str) -> String {
    match lang {
        Lang::ZhHans => format!("宿主机目录不存在:{path}"),
        Lang::EnUS => format!("host directory does not exist: {path}"),
    }
}

pub fn sync_pack_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("打包本地目录失败:{err}"),
        Lang::EnUS => format!("failed to pack the local directory: {err}"),
    }
}

pub fn sync_pack_task_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("打包任务异常:{err}"),
        Lang::EnUS => format!("packing task failed: {err}"),
    }
}

pub fn sync_pushed(lang: Lang, stats: &str, remote_dir: &str) -> String {
    match lang {
        Lang::ZhHans => format!("已推送 {stats} 到 {remote_dir}"),
        Lang::EnUS => format!("pushed {stats} to {remote_dir}"),
    }
}

pub fn sync_push_timeout(lang: Lang, seconds: u64) -> String {
    match lang {
        Lang::ZhHans => format!("推送超时({seconds}s)"),
        Lang::EnUS => format!("push timed out ({seconds}s)"),
    }
}

pub fn sync_push_cancelled(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "推送已被用户取消".to_string(),
        Lang::EnUS => "push cancelled by the user".to_string(),
    }
}

pub fn sync_remote_unpack_failed(lang: Lang, note: &str, output: &str) -> String {
    match lang {
        Lang::ZhHans => format!("远端解包失败({note}):{output}"),
        Lang::EnUS => format!("remote extraction failed ({note}): {output}"),
    }
}

pub fn sync_pull_timeout(lang: Lang, seconds: u64) -> String {
    match lang {
        Lang::ZhHans => format!("拉取超时({seconds}s)"),
        Lang::EnUS => format!("pull timed out ({seconds}s)"),
    }
}

pub fn sync_pulled(lang: Lang, files: u64, bytes: &str, target: &str) -> String {
    match lang {
        Lang::ZhHans => format!("已拉取 {files} 个文件 / {bytes} 到 {target}"),
        Lang::EnUS => format!("pulled {files} files / {bytes} to {target}"),
    }
}

pub fn sync_unpack_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("解包到宿主机失败:{err}"),
        Lang::EnUS => format!("failed to extract to the host: {err}"),
    }
}

pub fn sync_unpack_task_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("解包任务异常:{err}"),
        Lang::EnUS => format!("extraction task failed: {err}"),
    }
}

pub fn sync_pull_cancelled(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "拉取已被用户取消".to_string(),
        Lang::EnUS => "pull cancelled by the user".to_string(),
    }
}

pub fn sync_remote_pack_failed(lang: Lang, note: &str, output: &str) -> String {
    match lang {
        Lang::ZhHans => format!("远端打包失败({note}):{output}"),
        Lang::EnUS => format!("remote packing failed ({note}): {output}"),
    }
}

pub fn sync_archive_too_large(lang: Lang, limit: &str) -> String {
    match lang {
        Lang::ZhHans => format!("打包后超过 {limit} 上限,请先清理或加入排除项"),
        Lang::EnUS => {
            format!("the archive exceeds the {limit} limit; clean up or add exclusions first")
        }
    }
}

pub fn sync_clean_refused(lang: Lang, dir: &str) -> String {
    match lang {
        Lang::ZhHans => format!("拒绝在 {dir} 上执行覆写"),
        Lang::EnUS => format!("refusing to overwrite {dir}"),
    }
}

pub fn sync_clean_base_dir_refused(lang: Lang, dir: &str) -> String {
    match lang {
        Lang::ZhHans => format!("拒绝覆写基准目录本身({dir});覆写只针对具体项目目录"),
        Lang::EnUS => format!(
            "refusing to overwrite the base directory itself ({dir}); overwrite targets a \
             specific project directory only"
        ),
    }
}

// --- Path validation messages -----------------------------------------------
//
// Shown to the user in the WebUI (file tree / browse / task editor) and by the CLI.

pub fn path_side_invalid(lang: Lang, other: &str) -> String {
    match lang {
        Lang::ZhHans => format!("side 只能是 remote 或 host:{other}"),
        Lang::EnUS => format!("side must be remote or host: {other}"),
    }
}

pub fn path_empty(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "路径不能为空".to_string(),
        Lang::EnUS => "path must not be empty".to_string(),
    }
}

pub fn path_has_nul(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "路径里有空字节".to_string(),
        Lang::EnUS => "path contains a NUL byte".to_string(),
    }
}

pub fn path_absolute_refused(lang: Lang, raw: &str) -> String {
    match lang {
        Lang::ZhHans => format!("不允许绝对路径:{raw}"),
        Lang::EnUS => format!("absolute paths are not allowed: {raw}"),
    }
}

pub fn path_tilde_refused(lang: Lang, raw: &str) -> String {
    match lang {
        Lang::ZhHans => format!("不允许用 ~ 开头的路径:{raw}"),
        Lang::EnUS => format!("paths starting with ~ are not allowed: {raw}"),
    }
}

pub fn path_unc_refused(lang: Lang, raw: &str) -> String {
    match lang {
        Lang::ZhHans => format!("不允许 UNC 路径:{raw}"),
        Lang::EnUS => format!("UNC paths are not allowed: {raw}"),
    }
}

pub fn path_drive_refused(lang: Lang, raw: &str) -> String {
    match lang {
        Lang::ZhHans => format!("不允许带盘符的路径:{raw}"),
        Lang::EnUS => format!("paths with a drive letter are not allowed: {raw}"),
    }
}

pub fn path_dotdot_refused(lang: Lang, raw: &str) -> String {
    match lang {
        Lang::ZhHans => format!("路径里不允许出现 ..:{raw}"),
        Lang::EnUS => format!("`..` segments are not allowed in a path: {raw}"),
    }
}

pub fn path_escape(lang: Lang, relative: &str) -> String {
    match lang {
        Lang::ZhHans => format!("路径逃出了根目录:{relative}"),
        Lang::EnUS => format!("path escapes the root directory: {relative}"),
    }
}

pub fn path_absolute_required(lang: Lang, candidate: &str) -> String {
    match lang {
        Lang::ZhHans => format!("需要绝对路径:{candidate}"),
        Lang::EnUS => format!("an absolute path is required: {candidate}"),
    }
}

pub fn path_outside_browsable(lang: Lang, root: &str, candidate: &str) -> String {
    match lang {
        Lang::ZhHans => format!("只能浏览 {root} 下的内容:{candidate}"),
        Lang::EnUS => format!("only paths under {root} can be browsed: {candidate}"),
    }
}

pub fn path_no_home_or_roots(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "这台机器上找不到用户主目录,也没有配置 allowedRoots".to_string(),
        Lang::EnUS => {
            "no home directory could be found on this machine, and allowedRoots is not configured"
                .to_string()
        }
    }
}

pub fn path_outside_allowed_roots(lang: Lang, candidate: &str, allowed: &[String]) -> String {
    // The list separator is language-specific: `、` in Chinese, `, ` in English.
    let list = match lang {
        Lang::ZhHans => allowed.join("、"),
        Lang::EnUS => allowed.join(", "),
    };
    match lang {
        Lang::ZhHans => format!(
            "{candidate} 不在允许的目录里。允许的是:{list};要用别的目录,把它加进 config.json 的 allowedRoots。"
        ),
        Lang::EnUS => format!(
            "{candidate} is not in an allowed directory. Allowed: {list}; to use another one, \
             add it to allowedRoots in config.json."
        ),
    }
}

// --- HTTP client / log file / window / MCP status lines ----------------------

pub fn http_connect_failed(lang: Lang, addr: &str, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("连接 daemon({addr})失败:{err}"),
        Lang::EnUS => format!("failed to connect to the daemon ({addr}): {err}"),
    }
}

pub fn http_send_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("发送请求失败:{err}"),
        Lang::EnUS => format!("failed to send the request: {err}"),
    }
}

pub fn http_send_body_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("发送请求体失败:{err}"),
        Lang::EnUS => format!("failed to send the request body: {err}"),
    }
}

pub fn http_read_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("读取响应失败:{err}"),
        Lang::EnUS => format!("failed to read the response: {err}"),
    }
}

pub fn http_bad_response(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "响应格式不对(没有头部结束标记)".to_string(),
        Lang::EnUS => "malformed response (no end-of-headers marker)".to_string(),
    }
}

pub fn http_bad_status_line(lang: Lang, status_line: &str) -> String {
    match lang {
        Lang::ZhHans => format!("状态行不对:{status_line}"),
        Lang::EnUS => format!("malformed status line: {status_line}"),
    }
}

pub fn logfile_bad_date(lang: Lang, day: &str) -> String {
    match lang {
        Lang::ZhHans => format!("日期格式应当是 YYYY-MM-DD:{day}"),
        Lang::EnUS => format!("the date must be in YYYY-MM-DD form: {day}"),
    }
}

pub fn mcp_status_line(lang: Lang, url: &str, tools: usize) -> String {
    match lang {
        Lang::ZhHans => format!("{url} (POST {url}/mcp,{tools} 个工具)"),
        Lang::EnUS => format!("{url} (POST {url}/mcp, {tools} tools)"),
    }
}

pub fn window_open_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::ZhHans => format!("打开实例窗口失败:{err}"),
        Lang::EnUS => format!("failed to open the instance window: {err}"),
    }
}

// --- Environment probe (design doc §8) --------------------------------------

pub fn env_timeout(lang: Lang, seconds: u64) -> String {
    match lang {
        Lang::ZhHans => format!("环境检测超时({seconds}s)"),
        Lang::EnUS => format!("environment probe timed out ({seconds}s)"),
    }
}

pub fn env_cancelled(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "环境检测已取消".to_string(),
        Lang::EnUS => "environment probe cancelled".to_string(),
    }
}

pub fn env_no_output(lang: Lang, note: &str, output: &str) -> String {
    match lang {
        Lang::ZhHans => format!("没能从目标机拿到任何信息({note}):{output}"),
        Lang::EnUS => format!("got no information back from the target ({note}): {output}"),
    }
}

pub fn env_ready(lang: Lang, ready: usize, total: usize, extra: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{ready} / {total} 个工具就绪{extra}"),
        Lang::EnUS => format!("{ready} / {total} tools ready{extra}"),
    }
}

pub fn env_xcode_found(lang: Lang, count: usize) -> String {
    match lang {
        Lang::ZhHans => format!(",检测到 {count} 个 Xcode"),
        Lang::EnUS => format!(", {count} Xcode installation(s) detected"),
    }
}

// --- `cgw set` validation messages ------------------------------------------

pub fn set_unknown_category(lang: Lang, name: &str) -> String {
    match lang {
        Lang::ZhHans => format!(
            "未知分类:{name}。支持 port / listen / user / language / login-interval / log-retain / roots / mcp / host-lock / all"
        ),
        Lang::EnUS => format!(
            "unknown category: {name}. Supported: port / listen / user / language / login-interval / log-retain / roots / mcp / host-lock / all"
        ),
    }
}

pub fn set_missing_target(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "要改哪一侧?`set port --webui 65251` 或 `set port --mcp 3994`".to_string(),
        Lang::EnUS => {
            "which side? `set port --webui 65251` or `set port --mcp 3994`".to_string()
        }
    }
}

pub fn set_missing_value(lang: Lang, flag: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{flag} 后面要给值,例如 `set port {flag} 65251`"),
        Lang::EnUS => format!("{flag} needs a value, e.g. `set port {flag} 65251`"),
    }
}

pub fn set_bad_value(lang: Lang, what: &str, value: &str, want: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{what} 取值不对:{value}(要 {want})"),
        Lang::EnUS => format!("bad value for {what}: {value} (expected {want})"),
    }
}

pub fn set_nothing_to_change(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => {
            "要改什么?`set user --name <用户名> [--password <密码>]`,或 `--clear` 清掉".to_string()
        }
        Lang::EnUS => "what should change? `set user --name <user> [--password <pass>]`, or `--clear` to wipe it"
            .to_string(),
    }
}

pub fn set_need_path(lang: Lang, action: &str) -> String {
    match lang {
        Lang::ZhHans => format!("`set roots {action}` 后面要给至少一个绝对路径"),
        Lang::EnUS => format!("`set roots {action}` needs at least one absolute path"),
    }
}

pub fn set_unknown_action(lang: Lang, category: &str, action: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{category} 没有这个动作:{action}"),
        Lang::EnUS => format!("{category} has no such action: {action}"),
    }
}

/// The `what` label in a bad-value message (the field the user mistyped).
pub fn set_field_label(lang: Lang, field: &str) -> String {
    match (lang, field) {
        (Lang::ZhHans, "language") => "语言".to_string(),
        (Lang::ZhHans, "login-interval") => "登录限流间隔".to_string(),
        (Lang::ZhHans, "log-retain") => "日志保留天数".to_string(),
        (Lang::ZhHans, "host-lock") => "主机级互斥".to_string(),
        (Lang::ZhHans, "mcp") => "MCP 开关".to_string(),
        (Lang::ZhHans, "listen") => "监听范围".to_string(),
        (Lang::ZhHans, "port") => "端口".to_string(),
        (_, "language") => "language".to_string(),
        (_, "login-interval") => "login-interval".to_string(),
        (_, "log-retain") => "log-retain".to_string(),
        (_, "host-lock") => "host-lock".to_string(),
        (_, "mcp") => "mcp".to_string(),
        (_, "listen") => "listen".to_string(),
        (_, "port") => "port".to_string(),
        (_, other) => other.to_string(),
    }
}

/// The `want` half of a bad-value message.
///
/// Returns `&'static str` because [`crate::setcmd::SetError`] stores `want` that way.
pub fn set_expected(lang: Lang, field: &str) -> &'static str {
    match (lang, field) {
        (Lang::ZhHans, "login-interval") => "非负整数秒(0 = 不限流)",
        (Lang::ZhHans, "log-retain") => "非负整数(默认 7,上限 3650)",
        (Lang::ZhHans, "host-lock") => "on | off",
        (Lang::ZhHans, "mcp") => "on | off",
        (Lang::ZhHans, "port") => "0..65535 的整数(0 = 自动挑)",
        (Lang::ZhHans, "listen") => "lan | wan",
        (_, "login-interval") => "a non-negative number of seconds (0 = no throttling)",
        (_, "log-retain") => "a non-negative integer (default 7, max 3650)",
        (_, "host-lock") => "on | off",
        (_, "mcp") => "on | off",
        (_, "port") => "an integer in 0..65535 (0 = auto-pick)",
        (_, "listen") => "lan | wan",
        _ => "a valid value",
    }
}

// --- Headless daemon console (`compilegateway-daemon`) -----------------------
//
// These print before `app::bootstrap` resolves the config, so they rely on
// [`current`]'s system-locale fallback.

pub fn daemon_unknown_arg(lang: Lang, other: &str) -> String {
    match lang {
        Lang::ZhHans => format!("未知参数:{other}"),
        Lang::EnUS => format!("unknown argument: {other}"),
    }
}

pub fn daemon_listening(lang: Lang, app: &str, url: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{app} daemon 正在监听 {url}"),
        Lang::EnUS => format!("{app} daemon is listening on {url}"),
    }
}

pub fn daemon_ctrl_c(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhHans => "按 Ctrl+C 退出。",
        Lang::EnUS => "Press Ctrl+C to exit.",
    }
}

/// The headless daemon's `--help` body (after the app/version line).
pub fn daemon_help_lines(lang: Lang, app: &str) -> Vec<String> {
    match lang {
        Lang::ZhHans => vec![
            format!("用法: {app}-daemon [选项]"),
            String::new(),
            "  --open      启动后用系统默认浏览器打开 WebUI".to_string(),
            "  -h, --help  显示帮助".to_string(),
            "  -V, --version  显示版本".to_string(),
            String::new(),
            "配置读的是 exe 同目录的 config.json(找不到则回退当前工作目录)。".to_string(),
        ],
        Lang::EnUS => vec![
            format!("usage: {app}-daemon [options]"),
            String::new(),
            "  --open      open the WebUI in the system browser after startup".to_string(),
            "  -h, --help  show help".to_string(),
            "  -V, --version  show version".to_string(),
            String::new(),
            "The config is read from config.json next to the exe (falling back to the current \
             working directory)."
                .to_string(),
        ],
    }
}

// --- Classic-ZIP limits (export task) ---------------------------------------

pub fn zip_entry_too_large(lang: Lang, name: &str) -> String {
    match lang {
        Lang::ZhHans => format!("{name} 压缩后超过 4GiB,经典 ZIP 表达不了"),
        Lang::EnUS => format!("{name} exceeds 4GiB compressed, which classic ZIP cannot express"),
    }
}

pub fn zip_central_dir_too_large(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "中央目录超过 4GiB,经典 ZIP 表达不了".to_string(),
        Lang::EnUS => "the central directory exceeds 4GiB, which classic ZIP cannot express".to_string(),
    }
}

pub fn zip_too_many_entries(lang: Lang) -> String {
    match lang {
        Lang::ZhHans => "条目数超过 65535,经典 ZIP 表达不了".to_string(),
        Lang::EnUS => "there are more than 65535 entries, which classic ZIP cannot express".to_string(),
    }
}

// --- Layout -----------------------------------------------------------------

/// CJK characters count as two columns so zh and en labels line up.
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            let wide = matches!(u,
                0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
                | 0x20000..=0x3FFFD);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Pad a label to the given display width.
pub fn label(text: &str, width: usize) -> String {
    let w = display_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(w)))
}