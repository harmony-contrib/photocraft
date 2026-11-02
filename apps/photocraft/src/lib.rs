//! OpenHarmony host for the native PhotoCraft editor.
#![cfg(target_env = "ohos")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[allow(dead_code)]
mod control_server;
mod crash_guard;
#[allow(dead_code)]
mod gpu_startup;
#[allow(dead_code)]
mod logging;
mod ohos;
mod ui_state;

pub(crate) use ohos::services;
pub use ohos::start;
