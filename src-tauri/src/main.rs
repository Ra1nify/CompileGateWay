// Tauri shell: the app itself is a CLI-style window (log pane above, input below);
// the real GUI is the WebUI the daemon serves at the URL, opened in a browser.
//
// **All core logic lives in the `compilegateway` library crate**; this file only:
//   1. injects window capabilities (log push, native file picker) into the core via traits
//   2. builds the window and runs the boot flow
// The same core is used by `compilegateway-daemon` (headless) -- see src/bin/daemon.rs.
//
// Release builds carry no console window on Windows (logs go to the window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;

use std::sync::Arc;

use compilegateway::app::{self, BootOptions, APP_NAME, VERSION};
use compilegateway::dialog::{DialogDone, DialogKind, NativeDialog};
use compilegateway::log::{self, LogEntry, LogSink};
use tauri::{AppHandle, Emitter, Theme, WebviewUrl, WebviewWindowBuilder};

/// Log sink: the core pushes into `LogSink`, which forwards Tauri events to the right window.
struct TauriLogSink {
    app: AppHandle,
}

impl LogSink for TauriLogSink {
    fn emit(&self, scope: &str, entry: &LogEntry) {
        let _ = self.app.emit_to(scope, log::EVENT_LOG, entry.clone());
    }
}

/// Native file / folder picker (tauri-plugin-dialog).
struct TauriDialog {
    app: AppHandle,
}

impl NativeDialog for TauriDialog {
    fn pick(&self, kind: DialogKind, title: String, done: DialogDone) {
        use tauri_plugin_dialog::DialogExt;

        let builder = self.app.dialog().file().set_title(title);
        match kind {
            DialogKind::File => {
                builder.pick_file(move |path| done(path.map(|p| p.to_string())));
            }
            DialogKind::Folder => {
                builder.pick_folder(move |path| done(path.map(|p| p.to_string())));
            }
        }
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            cli::get_logs,
            cli::get_status,
            cli::run_command,
            cli::quit
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Attach the log sink first so boot logs after the window exists can be pushed
            log::core().attach(Arc::new(TauriLogSink { app: handle.clone() }));

            // CLI window: assets live in ../web and are embedded by tauri-build, no external server
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title(format!("{APP_NAME} {VERSION}"))
                .inner_size(940.0, 620.0)
                .min_inner_size(520.0, 320.0)
                .theme(Some(Theme::Dark))
                .center()
                .build()?;

            // Boot after the window exists so logs reach both the buffer (frontend snapshot) and the event stream
            app::bootstrap(BootOptions {
                open_browser: true,
                dialog: Some(Arc::new(TauriDialog { app: handle })),
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run CompileGateway");
}
