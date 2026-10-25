//! Native Ability registration; the editor lives in `photocraft`.
#![cfg(target_env = "ohos")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[openharmony_ability_derive::ability]
fn openharmony(app: openharmony_ability::OpenHarmonyApp) {
    if let Err(error) = photocraft::start(app) {
        log::error!("PhotoCraft startup failed: {error}");
    }
}
