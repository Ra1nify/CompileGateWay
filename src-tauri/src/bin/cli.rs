//! `cgw` -- the standalone CLI entry point. The implementation lives in [`compilegateway::client`].
//!
//! Why this entry is so thin: **one command implementation feeds three frontends** -- the
//! standalone CLI (this binary), the CLI window (the app itself) and the MCP `cgw://cli` table.
//! Keeping it in the library lets the window call the same code instead of a second copy
//! (which is why the window once lacked even `set`).

use std::process::ExitCode;

fn main() -> ExitCode {
    compilegateway::client::main_entry()
}
