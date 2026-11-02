//! OpenHarmony `photocraft_ui_egui::Services` construction.

use openharmony_ability::OpenHarmonyApp;
use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_format::RecoveryStore;
use photocraft_ui_egui::{Recoverable, Services};
use std::{
    cell::RefCell,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use winit::platform::ohos::{
    plugins::{clipboard::ClipboardClient, permission::PermissionExt as _, url::UrlExt as _},
    task,
};

const ENCODE_SHARE: f32 = 0.85;

pub fn prefs_file(directory: &Path) -> PathBuf {
    directory.join("preferences.json")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    photocraft_format::atomic_write(path, bytes).map_err(|e| e.to_string())
}

fn write_atomic_progress(path: &Path, bytes: &[u8], progress: &dyn Fn(f32)) -> Result<(), String> {
    photocraft_format::atomic_write_progress(path, bytes, progress).map_err(|e| e.to_string())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn same_entry(a: &Path, b: &Path) -> bool {
    let folder = |p: &Path| p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    if !same_file(a, b) || !same_file(&folder(a), &folder(b)) {
        return false;
    }
    let (Some(x), Some(y)) = (a.file_name(), b.file_name()) else { return false };
    if x == y {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(folder(a)) else { return false };
    let listed: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
    !(listed.iter().any(|n| n == x) && listed.iter().any(|n| n == y))
}

pub(crate) fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::park();
    }
}

fn save_through(
    doc: &Document,
    path: &str,
    settings: &photocraft_ui_egui::ExportSettings,
    ctl: &photocraft_ui_egui::jobs_ui::SaveCtl,
    write: impl Fn(&str, &[u8], &dyn Fn(f32)) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    ctl.progress(0.0, "Encoding");
    let opts = settings.io_options();
    let cancelled = || ctl.check().is_err();
    let progress = |f: f32| ctl.progress(f * ENCODE_SHARE, "");
    let interrupt = photocraft_raster::Interrupt::new(&cancelled, &progress);
    let r = crate::crash_guard::guard("Save", || photocraft_io::export_with(doc, path, &opts, &interrupt).map_err(|e| e.to_string()))?;
    ctl.commit()?;
    ctl.progress(ENCODE_SHARE, "Writing");
    write(path, &r.bytes, &|f| ctl.progress(ENCODE_SHARE + (0.99 - ENCODE_SHARE) * f, ""))?;
    ctl.progress(1.0, "");
    Ok(r.warnings)
}

fn save_file_service(ability: OpenHarmonyApp, grants: Arc<super::files::Grants>) -> photocraft_ui_egui::SaveFileFn {
    Arc::new(move |doc, path, settings, ctl| {
        let target = Path::new(path);
        if !grants.can_save(target) {
            ctl.progress(0.0, "Authorizing folder");
            if !block_on(grants.authorize_save(&ability, target))? {
                return Err("Save cancelled".into());
            }
        }
        save_through(doc, path, settings, ctl, |p, b, progress| write_atomic_progress(Path::new(p), b, progress))
    })
}

type SharedRecovery = Rc<RefCell<Option<RecoveryStore>>>;

fn with_store<R>(store: &SharedRecovery, f: impl FnOnce(&mut RecoveryStore) -> R) -> Result<R, String> {
    let mut slot = store.try_borrow_mut().map_err(|_| "crash recovery is busy".to_string())?;
    Ok(f(slot.as_mut().ok_or("no config directory")?))
}

fn recovery_services(dir: PathBuf) -> Services {
    let recover_dir = dir.clone();
    let store: SharedRecovery = Rc::new(RefCell::new(Some(RecoveryStore::new(dir))));
    let (s1, s2, s4, s5) = (store.clone(), store.clone(), store.clone(), store.clone());
    Services {
        autosave: Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
            with_store(&s1, |s| s.autosave_checked(doc, revision, path.map(str::to_string)))?
        })),
        autosave_results: Some(Box::new(move || with_store(&s4, |s| s.take_completed()).unwrap_or_default())),
        flush_autosaves: Some(Box::new(move || with_store(&s5, RecoveryStore::flush).unwrap_or_default())),
        discard_autosave: Some(Box::new(move |id: u64| {
            let _ = with_store(&s2, |s| s.discard(id));
        })),
        recover: Some(Box::new(move || {
            photocraft_format::list_recovery(&recover_dir)
                .into_iter()
                .map(|e| Recoverable {
                    key: e.info.key.clone(),
                    name: e.info.document_name.clone(),
                    path: e.info.original_path.clone(),
                    load: Box::new(move || photocraft_format::recover(&e).map_err(|e| e.to_string())),
                })
                .collect()
        })),
        adopt_autosave: Some(Box::new(move |id: u64, key: &str| {
            let _ = with_store(&store, |s| s.adopt(id, key));
        })),
        ..Default::default()
    }
}

pub fn build(
    ability: &OpenHarmonyApp,
    grants: &Arc<super::files::Grants>,
    egui_ctx: &egui::Context,
    events: std::sync::mpsc::Receiver<winit::platform::ohos::events::PlatformEvent>,
    cursor_pos: Option<photocraft_ui_egui::CursorPosFn>,
    screen_pick: Option<photocraft_ui_egui::screen_picker::Service>,
    automation: Option<photocraft_automation::AuthorizedWorkspace>,
) -> Services {
    let directory = grants.directory();
    let automation_read = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let bytes = workspace.read(path).map_err(|error| error.to_string())?;
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            Ok((name, bytes))
        }) as photocraft_ui_egui::AutomationReadFn
    });
    let automation_write = automation.clone().map(|workspace| {
        Box::new(move |path: &str, bytes: &[u8]| workspace.write(path, bytes).map_err(|error| error.to_string())) as photocraft_ui_egui::AutomationWriteFn
    });
    let automation_open_pcraft = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            let opened = crate::crash_guard::guard("Open", || workspace.open_pcraft(path).map_err(|e| e.to_string()))?;
            Ok(opened.map(|doc| (name, doc)))
        }) as photocraft_ui_egui::AutomationOpenPcraftFn
    });
    let automation_save = automation.clone().map(|workspace| {
        Arc::new(move |doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings, ctl: &photocraft_ui_egui::jobs_ui::SaveCtl| {
            save_through(doc, path, settings, ctl, |p, b, _| workspace.write(p, b).map_err(|e| e.to_string()))
        }) as photocraft_ui_egui::SaveFileFn
    });
    let step: fn(&str, &serde_json::Value) -> photocraft_engine::Result<()> = photocraft_automation::workspace::authorize_desktop_engine_step;
    let automation_authorize = automation.is_some().then_some(step);
    let automation_command = automation.map(|_| {
        Box::new(|id: &str, params: &serde_json::Value| {
            photocraft_automation::workspace::authorize_desktop_engine_command(id, params).map_err(|error| error.to_string())
        }) as photocraft_ui_egui::AutomationCommandFn
    });

    let prefs_load_path = prefs_file(directory);
    let prefs_save_path = prefs_load_path.clone();
    let url_app = ability.clone();
    let commands = Arc::new(task::CommandQueue::default());
    let clipboard_write_app = ability.clone();
    let writes = commands.clone();
    let clipboard_read_app = ability.clone();
    let clipboard_ctx = egui_ctx.clone();

    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            crate::crash_guard::guard("Open", || {
                photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
            })
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let opts = settings.io_options();
            crate::crash_guard::guard("Export", || photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string()))
        })),
        save_file: Some(save_file_service(ability.clone(), grants.clone())),
        file_dialog: Some(super::files::file_dialog_service(ability.clone(), grants.clone())),
        write: Some(Box::new(|path: &str, bytes: &[u8]| write_atomic(Path::new(path), bytes))),
        file_stamp: Some(Box::new(|path: &str| {
            let meta = std::fs::metadata(path).ok()?;
            Some(photocraft_ui_egui::FileStamp { len: meta.len(), modified: meta.modified().ok() })
        })),
        same_file: Some(Box::new(|a: &str, b: &str| same_entry(Path::new(a), Path::new(b)))),
        automation_read,
        automation_write,
        automation_open_pcraft,
        automation_save,
        automation_command,
        automation_authorize,
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        open_url: Some(Box::new(move |url| {
            let future = url_app.open_url(url);
            task::spawn(async move {
                if let Err(error) = future.await {
                    log::warn!("Open URL failed: {error}");
                }
            });
            Ok(())
        })),
        clipboard_set_image: Some(Box::new(move |width, height, bytes| {
            let app = clipboard_write_app.clone();
            let bytes = bytes.to_vec();
            writes.push(async move {
                let result = async { ClipboardClient::new(&app)?.write_image(&bytes, width, height).await }.await;
                if let Err(error) = result {
                    log::warn!("Image clipboard write failed: {error}");
                }
            });
            Ok(())
        })),
        clipboard_read_async: Some(Box::new(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let app = clipboard_read_app.clone();
            let ctx = clipboard_ctx.clone();
            commands.push(async move {
                let result: Result<_, String> = async {
                    app.request_permission("ohos.permission.READ_PASTEBOARD").await.map_err(|error| error.to_string())?;
                    let contents = ClipboardClient::new(&app).map_err(|error| error.to_string())?.read_content().await.map_err(|error| error.to_string())?;
                    let png = match contents.png {
                        Some(png) => Some(png),
                        None => {
                            contents.uris.iter().filter_map(|uri| winit::platform::ohos::drag_drop::file_path(uri)).find_map(|path| std::fs::read(path).ok())
                        }
                    };
                    png.map(|bytes| {
                        let image = photocraft_codecs::decode(&bytes).map_err(|error| error.to_string())?;
                        Ok((image.width(), image.height(), image.to_rgba8()))
                    })
                    .transpose()
                }
                .await;
                let _ = tx.send(result);
                ctx.request_repaint();
            });
            rx
        })),
        os_events: Some(Box::new(move || {
            events
                .try_iter()
                .filter_map(|event| match event {
                    winit::platform::ohos::events::PlatformEvent::Want { uri, .. } if !uri.is_empty() => winit::platform::ohos::drag_drop::file_path(&uri)
                        .map(|path| photocraft_ui_egui::OsEvent::Open(vec![path.to_string_lossy().into_owned()])),
                    _ => None,
                })
                .collect()
        })),
        load_prefs: Some(Box::new(move || std::fs::read_to_string(&prefs_load_path).ok())),
        save_prefs: Some(Box::new(move |text: &str| write_atomic(&prefs_save_path, text.as_bytes()))),
        append_text: Some(Box::new(|path: &str, text: &str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())
        })),
        preset_store: Some(photocraft_engine::preset_store::open_dir_async(directory.join("Presets"))),
        cursor_pos,
        screen_pick,
        ..recovery_services(directory.join("Recovery"))
    }
}
