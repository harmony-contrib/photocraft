//! OpenHarmony host for the native PhotoCraft editor.
#![cfg(target_env = "ohos")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
mod app_dirs {
    use std::{path::PathBuf, sync::OnceLock};
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    pub fn set_ohos_dir(path: PathBuf) {
        let _ = DIRECTORY.set(path);
    }
    pub fn config_dir() -> Option<PathBuf> {
        DIRECTORY.get().cloned()
    }
    pub fn recovery_dir() -> Option<PathBuf> {
        config_dir().map(|d| d.join("Recovery"))
    }
}
mod control_server;
mod crash_guard;
mod cursor;
mod gpu_startup;
mod logging;
mod monitor_profile;
mod ohos;
mod ohos_files;
mod screen_color;
mod services;
mod ui_state;

pub use ohos::start;
