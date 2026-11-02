//! OpenHarmony Ability lifecycle host and platform integrations.
//!
//! All OpenHarmony-specific host glue (file grants/dialogs, pasteboard, URL launcher, display ICC
//! detection, window snapshot colour picking, pointer drop position, and HiLog forwarding) lives
//! under `ohos/` so desktop modules remain untouched.

mod display;
mod files;
pub(crate) mod services;

use openharmony_ability::OpenHarmonyApp;
use photocraft_engine::Session;
use photocraft_ui_egui::{CursorPosFn, PhotocraftApp};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::SystemTime,
};

struct OhosLogger(crate::logging::AppLogger);

impl log::Log for OhosLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.0.log(record);
        let thread = std::thread::current();
        let line = crate::logging::format_line(
            &crate::logging::timestamp(SystemTime::now()),
            record.level(),
            thread.name().unwrap_or("?"),
            record.target(),
            &record.args().to_string(),
        );
        let hilog_msg = line.trim_end_matches('\n').replace('\0', "");
        match record.level() {
            log::Level::Error => ohos_hilog_binding::error(hilog_msg),
            log::Level::Warn => ohos_hilog_binding::warn(hilog_msg),
            log::Level::Info => ohos_hilog_binding::info(hilog_msg),
            log::Level::Debug | log::Level::Trace => ohos_hilog_binding::debug(hilog_msg),
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

fn install_logger() -> Option<&'static crate::logging::AppLogger> {
    static LOGGER: OnceLock<OhosLogger> = OnceLock::new();
    let spec = std::env::var("RUST_LOG").ok().filter(|s| !s.trim().is_empty());
    let filter = crate::logging::Filter::parse(spec.as_deref().unwrap_or(crate::logging::DEFAULT_FILTER));
    let max = filter.max();
    let logger = LOGGER.get_or_init(|| OhosLogger(crate::logging::AppLogger::new(filter, true)));
    log::set_logger(logger).ok()?;
    log::set_max_level(max);
    Some(&logger.0)
}

fn cursor_service(_cc: &eframe::CreationContext<'_>) -> Option<CursorPosFn> {
    Some(Box::new(|ctx: &egui::Context| {
        if let Some((x, y)) = winit::platform::ohos::drag_drop::last_position() {
            let ppp = ctx.pixels_per_point();
            if ppp.is_finite() && ppp > 0.0 {
                return Some(egui::pos2(x as f32 / ppp, y as f32 / ppp));
            }
        }
        ctx.input(|i| i.pointer.latest_pos())
    }))
}

pub fn start(ability: OpenHarmonyApp) -> Result<(), String> {
    let logger = install_logger();
    crate::crash_guard::install_hook();
    let directory = ability.pref_path().or_else(|| ability.base_path()).map(PathBuf::from).ok_or("Ability did not provide an application data directory")?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    if let Some(logger) = logger {
        match logger.attach_dir(&directory.join("logs")) {
            Ok(path) => log::info!("PhotoCraft {}, log file {}", photocraft_engine::build_info::long_version(), path.display()),
            Err(e) => log::warn!("no log file: {e}"),
        }
    }
    let monitor = display::detect_async();
    // Fontique scans all installed families. On an emulator this can take several
    // seconds; finish it before allowing text tools/imports to touch the shared DB.
    let grants = Arc::new(files::Grants::new(directory.clone()));
    let startup_grants = grants.clone();
    let (fonts_tx, fonts_ready) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("photocraft-fonts".into())
        .spawn(move || {
            startup_grants.restore();
            let result = std::panic::catch_unwind(photocraft_ui_egui::type_tool::families)
                .map(|_| ())
                .map_err(|_| "Unable to load installed fonts. Restart PhotoCraft to retry.".to_owned());
            let _ = fonts_tx.send(result);
        })
        .map_err(|error| error.to_string())?;
    let mut options = eframe::NativeOptions {
        openharmony_app: Some(ability.clone()),
        viewport: egui::ViewportBuilder::default()
            .with_title("PhotoCraft")
            .with_app_id("ai.storyteller.photocraft")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true),
        persistence_path: Some(directory.join("ui.ron")),
        system_font_fallback: false,
        ..Default::default()
    };
    crate::ui_state::sanitize(options.persistence_path.as_deref());
    options.centered = !crate::ui_state::has_saved_position(options.persistence_path.as_deref());
    let t_sentinel = std::time::Instant::now();
    let os = crate::gpu_startup::Os::current();
    let prefs = services::prefs_file(&directory);
    let (pref, mode) = crate::gpu_startup::read_rendering_prefs(Some(&prefs));
    let (previous, sentinel) = crate::gpu_startup::Sentinel::begin(&directory);
    let plan = crate::gpu_startup::plan_with_mode(pref, mode, previous.crashed(), None, false, os);
    if let Some(m) = previous.crashed() {
        log::warn!("the previous start didn't finish (GPU backend {}, adapter {:?}); {}", m.backend, m.adapter, plan.reason.as_deref().unwrap_or(""));
    }
    let sentinel: crate::gpu_startup::SharedSentinel = Arc::new(std::sync::Mutex::new(sentinel));
    if let Some(s) = sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
        let marker = crate::gpu_startup::Marker {
            backend: plan.backend.name().to_string(),
            version: photocraft_engine::build_info::long_version().to_string(),
            ..Default::default()
        };
        if let Err(e) = s.write(marker) {
            log::warn!("GPU startup marker: {e}");
        }
    }
    let gpu_note: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let wanted_adapter =
        crate::gpu_startup::wanted_adapter(std::env::var("PHOTOCRAFT_GPU_ADAPTER").ok().as_deref(), &crate::gpu_startup::read_gpu_adapter(Some(&prefs)));
    crate::gpu_startup::configure(&mut options.wgpu_options.wgpu_setup, &plan, os, wanted_adapter, sentinel.clone(), gpu_note.clone());
    if crate::gpu_startup::read_low_latency(Some(&prefs)) {
        options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    }
    // API 26's emulator exposes GLES; the editor can still use its wgpu canvas on this backend.
    if plan.env.is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    let sentinel_ms = t_sentinel.elapsed().as_secs_f64() * 1000.0;
    log::info!("GPU startup: {:?} ({sentinel_ms:.2} ms)", plan);
    let keep_marker = crate::gpu_startup::keep_marker_after_error(&plan, os);
    let started_sentinel = sentinel.clone();
    #[cfg(feature = "ohos-control")]
    let control = {
        let root = directory.join("validation");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let workspace = photocraft_automation::AuthorizedWorkspace::new(Some(&root), Some(&root)).map_err(|error| error.to_string())?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 7878)).map_err(|error| error.to_string())?;
        let token = include_str!(env!("PHOTOCRAFT_OHOS_CONTROL_TOKEN_FILE")).trim().to_owned();
        Some((listener, workspace, token))
    };
    #[cfg(not(feature = "ohos-control"))]
    let control: Option<(std::net::TcpListener, photocraft_automation::AuthorizedWorkspace, String)> = None;
    // The launch Want can arrive before eframe creates the first rendering surface.
    let events = winit::platform::ohos::events::subscribe();
    let result = eframe::run_native(
        "PhotoCraft",
        options,
        Box::new(move |cc| {
            // Keep floating panels embedded in the single OpenHarmony ability window.
            cc.egui_ctx.set_embed_viewports(true);
            PhotocraftApp::install_type_reset_hooks(&cc.egui_ctx);
            let egui_ctx = cc.egui_ctx.clone();
            let render_state = cc.wgpu_render_state.clone();
            let cursor_pos = cursor_service(cc);
            let screen_pick = display::screen_color_service(cc);
            let create = move || {
                let services =
                    services::build(&ability, &grants, &egui_ctx, events, cursor_pos, screen_pick, control.as_ref().map(|(_, workspace, _)| workspace.clone()));
                let mut app = PhotocraftApp::new(Session::new(), services);
                app.background_jobs = true;
                if let Some(rx) = monitor {
                    app.services.read_displays = Some(Arc::new(display::detect_async));
                    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                        Ok(r) => photocraft_ui_egui::monitor_status::apply(&mut app, r),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => photocraft_ui_egui::monitor_status::pending(&mut app, rx),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            photocraft_ui_egui::monitor_status::apply(&mut app, Err("the display profile reader stopped without an answer".into()));
                        }
                    }
                }
                app.perf.gpu_info.preference = pref.name().to_string();
                app.perf.gpu_info.selected = if plan.env.is_some() { "env".into() } else { plan.backend.name().to_string() };
                app.perf.gpu_info.fallback = match (plan.reason.clone(), gpu_note.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()) {
                    (Some(reason), Some(note)) => Some(format!("{reason}. {note}")),
                    (reason, note) => reason.or(note),
                };
                app.perf.gpu_info.canvas = "cpu".into();
                if let Some(rs) = render_state.clone() {
                    app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
                    app.perf.gpu_info.set_available(&rs.available_adapters);
                    photocraft_ui_egui::texel_guard::install(&egui_ctx, &rs.adapter.get_info());
                    if plan.backend != photocraft_engine::prefs::GpuBackend::Cpu
                        && app.session.prefs().performance.effective_rendering_mode() != photocraft_engine::prefs::RenderingMode::Cpu
                    {
                        app.perf.gpu_info.selected = "gl".into();
                        app.set_wgpu(rs);
                    } else {
                        app.watch_window_device(&rs.device);
                    }
                }
                if app.perf.gpu_info.canvas == "cpu"
                    && mode != photocraft_engine::prefs::RenderingMode::Cpu
                    && let Some(reason) = app.perf.gpu_info.fallback.clone()
                {
                    photocraft_ui_egui::gpu_status::queue_fallback_notice(&mut app, &reason);
                }
                app.perf.span("gpuSentinel", sentinel_ms);
                let remember = crate::gpu_startup::backend_to_remember(&plan);
                app.on_started(move |app| {
                    if let Some(s) = started_sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                        s.finish();
                    }
                    if let Some(b) = remember
                        && app.session.prefs().performance.gpu_backend != b
                        && let Err(e) = app.run("prefs.set", serde_json::json!({"path": "performance.gpuBackend", "value": b.name()}))
                    {
                        log::warn!("couldn't remember GPU backend {}: {e}", b.name());
                    }
                });
                if let Some((listener, _, token)) = control {
                    app = app.with_control(crate::control_server::start(listener, token, egui_ctx.clone()));
                }
                log::info!("PhotoCraft native editor ready");
                app
            };
            Ok(Box::new(StartupApp { fonts_ready, create: Some(Box::new(create)), app: None, error: None }))
        }),
    );
    let renderer_error = crate::gpu_startup::keep_marker_after_run(&result);
    if !(renderer_error && keep_marker)
        && let Some(mut s) = sentinel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    {
        if result.is_err()
            && !renderer_error
            && let Some(marker) = previous.crashed()
        {
            if let Err(error) = s.write(marker.clone()) {
                log::warn!("couldn't restore previous GPU startup marker: {error}");
            }
        } else {
            s.finish();
        }
    }
    result.map_err(|error| error.to_string())
}

/// Keep the Ability's UI thread responsive while the native font catalogue loads.
struct StartupApp {
    fonts_ready: std::sync::mpsc::Receiver<Result<(), String>>,
    create: Option<Box<dyn FnOnce() -> PhotocraftApp>>,
    app: Option<PhotocraftApp>,
    error: Option<String>,
}

impl eframe::App for StartupApp {
    fn on_exit(&mut self) {
        if let Some(app) = &mut self.app {
            app.on_exit();
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.app.is_none() && self.error.is_none() {
            match self.fonts_ready.try_recv() {
                Ok(Ok(())) => self.app = self.create.take().map(|create| create()),
                Ok(Err(error)) => self.error = Some(error),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.error = Some("Font loading stopped. Restart PhotoCraft to retry.".into()),
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        if let Some(app) = &mut self.app {
            app.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(app) = &mut self.app {
            app.ui(ui, frame);
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.4);
                    ui.heading("PhotoCraft");
                    if let Some(error) = &self.error {
                        ui.label(error);
                    } else {
                        ui.spinner();
                        ui.label("Loading installed fonts…");
                    }
                });
            });
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if let Some(app) = &mut self.app {
            app.raw_input_hook(ctx, input);
        }
    }
}
