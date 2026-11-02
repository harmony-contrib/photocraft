//! OpenHarmony file picker, save filters, and persistent URI grants (`file-grants.json`).
//!
//! Atomic replacement needs the containing directory: a single-file grant cannot create a sibling
//! temporary file.

use openharmony_ability::OpenHarmonyApp;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogFn, FileDialogRequest};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};
use winit::platform::ohos::{
    file_access::{self, permissions},
    plugins::files::{FileDialogFilter, FileDialogOptions, FilesExt as _},
    task,
};

/// Everything File › Open reads on OpenHarmony.
const OPEN_EXTS: &[&str] = &[
    "pcraft", "pdn", "ora", "xcf", "xcfgz", "gz", "kra", "krz", "psd", "psb", "psdt", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "dib", "tga",
    "ico", "qoi", "exr", "hdr", "pbm", "pgm", "ppm", "pnm", "pam", "pfm", "pcx", "dds", "xpm", "fits", "fit", "fts", "dpx", "cin", "dcm", "dicom", "dc3",
    "iff", "lbm", "ilbm", "heic", "heif", "hif", "dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf", "abr", "grd", "asl", "svg", "svgz",
    "aco", "act", "ase", "kys", "af", "afdesign", "afphoto", "afpub",
];

const SAVE_FILTERS: &[(&str, &[&str])] = &[
    ("PSD Document", &["psd", "psb"]),
    ("PDF", &["pdf"]),
    ("PhotoCraft", &["pcraft"]),
    ("OpenRaster", &["ora"]),
    ("PNG", &["png"]),
    ("JPEG", &["jpg", "jpeg"]),
    ("JPEG XL", &["jxl"]),
    ("WebP", &["webp"]),
    ("TIFF", &["tif", "tiff"]),
    ("Targa", &["tga"]),
    ("BMP", &["bmp", "dib"]),
    ("GIF", &["gif"]),
    ("ICO", &["ico"]),
    ("OpenEXR", &["exr"]),
    ("Netpbm", &["pnm"]),
    ("Radiance HDR", &["hdr"]),
    ("FITS", &["fits", "fit"]),
    ("DPX", &["dpx"]),
    ("DDS", &["dds"]),
    ("Cineon", &["cin"]),
    ("Amiga IFF", &["iff"]),
];

const OTHER_SAVE_FILTERS: &[(&str, &[&str])] = &[("Color Swatches", &["aco"]), ("Color Table", &["act"]), ("Swatch Exchange", &["ase"])];

// A directory read/write grant covers creating and replacing its children.
const SAVE_FOLDER_MODE: u32 = 1 | 2;

pub struct Grants {
    directory: PathBuf,
    uris: Mutex<BTreeMap<String, u32>>,
    writable: Mutex<Vec<PathBuf>>,
}

impl Grants {
    pub fn new(directory: PathBuf) -> Self {
        Self { writable: Mutex::new(vec![directory.clone()]), directory, uris: Mutex::new(BTreeMap::new()) }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Called on the startup worker, before recent files can be opened.
    pub fn restore(&self) {
        let saved = std::fs::read(self.directory.join("file-grants.json"));
        let saved: BTreeMap<String, u32> = match saved {
            Ok(bytes) if bytes.len() <= 1024 * 1024 => serde_json::from_slice(&bytes).unwrap_or_default(),
            _ => return,
        };
        for (uri, mode) in saved {
            if !matches!(mode, 1 | 3) {
                continue;
            }
            let policy = permissions::PolicyInfo { uri: uri.clone(), operation_mode: mode };
            match permissions::activate_permission(&[policy]) {
                Ok(failed) if failed.is_empty() => self.track(&uri, mode),
                _ => log::warn!("A saved file grant is no longer available; select the file again"),
            }
        }
    }

    fn track(&self, uri: &str, mode: u32) {
        self.uris.lock().unwrap_or_else(PoisonError::into_inner).entry(uri.to_owned()).and_modify(|old| *old |= mode).or_insert(mode);
        if mode == SAVE_FOLDER_MODE
            && let Some(path) = winit::platform::ohos::drag_drop::file_path(uri).filter(|p| p.is_dir())
        {
            let mut writable = self.writable.lock().unwrap_or_else(PoisonError::into_inner);
            if !writable.contains(&path) {
                writable.push(path);
            }
        }
    }

    pub fn persist(&self, uris: &[String], write: bool) -> Result<Vec<PathBuf>, String> {
        let paths = uris
            .iter()
            .map(|uri| winit::platform::ohos::drag_drop::file_path(uri).ok_or_else(|| "Cannot resolve selected URI".to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        // Avoid redundant file policies inside an explicitly selected writable folder.
        let individual: Vec<String> = uris.iter().zip(&paths).filter(|(_, path)| !self.can_save(path)).map(|(uri, _)| uri.clone()).collect();
        file_access::persist_paths(&individual, write)?;
        for uri in &individual {
            let mode = if write { SAVE_FOLDER_MODE } else { 1 };
            self.track(uri, mode);
        }
        let uris = self.uris.lock().unwrap_or_else(PoisonError::into_inner);
        let bytes = serde_json::to_vec(&*uris).map_err(|e| e.to_string())?;
        photocraft_format::atomic_write(&self.directory.join("file-grants.json"), &bytes).map_err(|e| e.to_string())?;
        Ok(paths)
    }

    pub fn can_save(&self, path: &Path) -> bool {
        path.parent().is_some_and(|parent| self.writable.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|dir| parent.starts_with(dir)))
    }

    /// The OS picker is the user's explicit authorization for this directory.
    /// Cancellation leaves the original document and destination untouched.
    pub async fn authorize_save(&self, app: &OpenHarmonyApp, path: &Path) -> Result<bool, String> {
        if self.can_save(path) {
            return Ok(true);
        }
        let parent = path.parent().ok_or("The save path has no containing folder")?;
        let uri = winit::platform::ohos::drag_drop::file_uri(parent).ok_or("Cannot resolve the save folder URI")?;
        let response = app.show_file_dialog(FileDialogOptions::new("open-folder").default_location(uri)).await.map_err(|e| e.to_string())?;
        let Some(selected) = response.files.first() else { return Ok(false) };
        if winit::platform::ohos::drag_drop::file_path(selected).as_deref() != Some(parent) {
            return Err(format!("Select {} to save this file safely, or use Save As to choose another location", parent.display()));
        }
        self.persist(&response.files, true)?;
        Ok(true)
    }
}

fn netpbm_as_pnm(suggested: &str) -> String {
    let path = Path::new(suggested);
    let subtype = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).is_some_and(|e| ["pbm", "pgm", "ppm", "pam", "pfm"].contains(&e.as_str()));
    if subtype { path.with_extension("pnm").to_string_lossy().into_owned() } else { suggested.to_string() }
}

fn save_filters(suggested: &str) -> Vec<(String, Vec<String>)> {
    let ext = Path::new(suggested).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let ext = if ["pbm", "pgm", "ppm", "pam", "pfm"].contains(&ext.as_str()) { "pnm".to_string() } else { ext };
    if let Some((name, exts)) = OTHER_SAVE_FILTERS.iter().find(|(_, exts)| exts.contains(&ext.as_str())) {
        return vec![(name.to_string(), exts.iter().map(|e| e.to_string()).collect())];
    }
    let mut v: Vec<(String, Vec<String>)> = SAVE_FILTERS.iter().map(|(name, exts)| (name.to_string(), exts.iter().map(|e| e.to_string()).collect())).collect();
    match v.iter().position(|(_, exts)| exts.contains(&ext)) {
        Some(i) => {
            let (name, mut exts) = v.remove(i);
            exts.retain(|e| *e != ext);
            exts.insert(0, ext);
            v.insert(0, (name, exts));
        }
        None if !ext.is_empty() => v.insert(0, (ext.to_ascii_uppercase(), vec![ext])),
        None => {}
    }
    v
}

pub fn file_dialog_service(ability: OpenHarmonyApp, grants: Arc<Grants>) -> FileDialogFn {
    Box::new(move |request, _parent, reply| {
        let app = ability.clone();
        let grants = grants.clone();
        task::spawn(async move {
            let result: Result<Option<FileDialogAnswer>, String> = async {
                enum DialogKind {
                    Open,
                    Folder,
                    Save,
                }
                let with_dir = |options: FileDialogOptions, dir: Option<String>| match dir
                    .as_deref()
                    .and_then(|d| winit::platform::ohos::drag_drop::file_uri(Path::new(d)))
                {
                    Some(uri) => options.default_location(uri),
                    None => options,
                };
                let (kind, options) = match request {
                    FileDialogRequest::Open { multiple, initial_dir, extensions } => {
                        let filter = match extensions {
                            Some(exts) => FileDialogFilter::new().name("Supported Files").pattern(exts.join(";")),
                            None => FileDialogFilter::new().name("All Formats").pattern(OPEN_EXTS.join(";")),
                        };
                        (DialogKind::Open, with_dir(FileDialogOptions::new("open-file").allow_many(multiple).filters(vec![filter]), initial_dir))
                    }
                    FileDialogRequest::Folder { initial_dir } => (DialogKind::Folder, with_dir(FileDialogOptions::new("open-folder"), initial_dir)),
                    FileDialogRequest::Save { suggested } => {
                        let suggested = netpbm_as_pnm(&suggested);
                        let options = FileDialogOptions::new("save-file")
                            .filters(
                                save_filters(&suggested)
                                    .into_iter()
                                    .map(|(name, extensions)| FileDialogFilter::new().name(name).pattern(extensions.join(";")))
                                    .collect(),
                            )
                            .suggested_name(
                                Path::new(&suggested).file_name().map_or_else(|| "Untitled.psd".into(), |name| name.to_string_lossy().into_owned()),
                            );
                        (DialogKind::Save, options)
                    }
                };
                let response = app.show_file_dialog(options).await.map_err(|error| error.to_string())?;
                match kind {
                    DialogKind::Save => {
                        let Some(uri) = response.files.first() else { return Ok(None) };
                        let path = winit::platform::ohos::drag_drop::file_path(uri).ok_or("Cannot resolve selected save URI")?;
                        Ok(grants.authorize_save(&app, &path).await?.then(|| FileDialogAnswer::SaveTo(path.to_string_lossy().into_owned())))
                    }
                    DialogKind::Folder => {
                        let paths = grants.persist(&response.files, true)?;
                        Ok(paths.into_iter().next().map(|p| FileDialogAnswer::FolderTo(p.to_string_lossy().into_owned())))
                    }
                    DialogKind::Open => {
                        let paths = grants.persist(&response.files, false)?;
                        Ok((!paths.is_empty()).then(|| FileDialogAnswer::Paths(paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect())))
                    }
                }
            }
            .await;
            match result {
                Ok(answer) => reply.send(answer),
                Err(error) => {
                    log::error!("PhotoCraft file dialog failed: {error}");
                    reply.send(Some(FileDialogAnswer::Unavailable(error)));
                }
            }
        });
    })
}
