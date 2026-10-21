//! CompileGateway core logic (library crate).
//!
//! **No Tauri here**: window and dialog capabilities are injected by the shell through
//! traits ([`log::LogSink`], [`dialog::NativeDialog`]), so the same core serves both the
//! windowed Tauri app and the headless daemon (design doc §2).
//!
//! Two binary targets:
//!   * `compilegateway` -- the Tauri shell (CLI window + WebUI)
//!   * `compilegateway-daemon` -- CLI + HTTP only, runs on any Linux box with no GUI

pub mod app;
pub mod auth;
pub mod build;
pub mod clients;
pub mod client;
pub mod config;
pub mod contract;
pub mod dialog;
pub mod env;
pub mod fileops;
pub mod help;
pub mod httpclient;
pub mod i18n;
pub mod jobs;
pub mod jsonschema;
pub mod known_hosts;
pub mod lock;
pub mod log;
pub mod logfile;
pub mod manifest;
pub mod mcp;
pub mod net;
pub mod paths;
pub mod projects;
pub mod server;
pub mod setcmd;
pub mod ssh;
pub mod state;
pub mod sync;
pub mod theme;
pub mod tree;
pub mod workflow;
pub mod zip;