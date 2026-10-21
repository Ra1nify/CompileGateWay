//! Headless daemon: no window, no webview, just config loading + HTTP serving.
//!
//! The other half of design doc §2 -- runs on an always-on box with no GUI, sharing the
//! same core logic as the Tauri build. The only differences:
//!   * no `LogSink` (nothing to push to); logs stay buffered and are read via `/api/clients/{id}/logs`
//!   * no `NativeDialog`; file-pick endpoints report that this environment has no dialog
//!   * the browser is not opened automatically

#![cfg_attr(not(debug_assertions), windows_subsystem = "console")]

use std::process::ExitCode;

use compilegateway::app::{self, BootOptions, APP_NAME, VERSION};

fn main() -> ExitCode {
    let mut open_browser = false;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--open" => open_browser = true,
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("{APP_NAME} {VERSION} (daemon)");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!(
                    "{}",
                    compilegateway::i18n::daemon_unknown_arg(
                        compilegateway::i18n::current(),
                        other
                    )
                );
                print_help();
                return ExitCode::from(2);
            }
        }
    }

    let booted = app::bootstrap(BootOptions {
        open_browser,
        dialog: None,
    });

    // On startup failure (port in use / broken config) the log already has the reason; exit non-zero
    if booted.fatal {
        return ExitCode::FAILURE;
    }

    let lang = compilegateway::i18n::current();
    println!(
        "{}",
        compilegateway::i18n::daemon_listening(lang, APP_NAME, &booted.url)
    );
    println!("{}", compilegateway::i18n::daemon_ctrl_c(lang));

    // The HTTP server runs on its own thread; the main thread waits here
    loop {
        std::thread::park();
    }
}

fn print_help() {
    println!("{APP_NAME} {VERSION} (headless daemon)");
    for line in
        compilegateway::i18n::daemon_help_lines(compilegateway::i18n::current(), APP_NAME)
    {
        println!("{line}");
    }
}
