//! Native file-picker abstraction.
//!
//! The library crate does not know Tauri: the windowed shell injects a
//! [`NativeDialog`] backed by `tauri-plugin-dialog`, headless daemon injects
//! nothing, and the call then reports that no dialog exists here.
//!
//! Not a browser call: WebUI runs in a browser and has no Tauri APIs, and the
//! path is read by the daemon for private keys / source lookup, so the picker
//! must target the **daemon's own machine** (design doc §6.1).

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    File,
    Folder,
}

/// Called on the main thread after the user picks; `None` means cancelled.
pub type DialogDone = Box<dyn FnOnce(Option<String>) + Send + 'static>;

pub trait NativeDialog: Send + Sync {
    fn pick(&self, kind: DialogKind, title: String, done: DialogDone);
}

pub type SharedDialog = Arc<dyn NativeDialog>;
