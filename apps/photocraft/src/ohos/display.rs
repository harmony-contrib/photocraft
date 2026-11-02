//! OpenHarmony display ICC profile detection and window snapshot colour picking.

use photocraft_engine::display_color::Display;
use photocraft_ui_egui::screen_picker::{Capture, MAX_PIXELS, Pending, ScreenImage, Service};
use std::sync::mpsc::Receiver;

pub type Detection = Result<Vec<Display>, String>;

pub fn detect_async() -> Option<Receiver<Detection>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(detect());
    });
    Some(rx)
}

pub fn detect() -> Detection {
    let raw_displays = openharmony_ability::display::all_displays()
        .ok()
        .filter(|list| !list.is_empty())
        .or_else(|| openharmony_ability::display::primary_display().ok().map(|d| vec![d]));

    let mut displays = Vec::new();
    if let Some(raw_displays) = raw_displays {
        for info in raw_displays {
            if info.width <= 0 || info.height <= 0 {
                continue;
            }
            let (px_x, px_y) = openharmony_ability::display::display_position(u64::from(info.id)).unwrap_or((0, 0));
            let scale = if info.density_pixels.is_finite() && info.density_pixels > 0.0 { f64::from(info.density_pixels) } else { 1.0 };
            let frame = [f64::from(px_x) / scale, f64::from(px_y) / scale, f64::from(info.width) / scale, f64::from(info.height) / scale];
            let trimmed = info.name.trim();
            let name = if trimmed.is_empty() { format!("Display {}", info.id) } else { trimmed.to_owned() };
            let (builtin, profile_label) = ohos_color_space_builtin(&info.color_spaces);
            displays.push(Display { id: info.id, name, frame, profile_name: Some(profile_label.to_owned()), icc: Some(builtin.profile().to_bytes()) });
        }
    }

    if displays.is_empty() {
        let id = u32::try_from(openharmony_ability::display::default_display_id()).unwrap_or(0);
        let width = openharmony_ability::display::default_display_width();
        let height = openharmony_ability::display::default_display_height();
        if width <= 0 || height <= 0 {
            return Err("OpenHarmony display manager reported no active display".into());
        }
        let scale = f64::from(openharmony_ability::display::default_display_virtual_pixel_ratio());
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let (builtin, profile_label) = ohos_color_space_builtin(&[]);
        displays.push(Display {
            id,
            name: format!("Display {id}"),
            frame: [0.0, 0.0, f64::from(width) / scale, f64::from(height) / scale],
            profile_name: Some(profile_label.to_owned()),
            icc: Some(builtin.profile().to_bytes()),
        });
    }
    Ok(displays)
}

pub fn profile_for_display(display_id: u32) -> Option<Vec<u8>> {
    let displays = detect().ok()?;
    let matched = displays.iter().find(|d| d.id == display_id).or_else(|| displays.first())?;
    matched.icc.as_ref().map(|arc| (**arc).clone())
}

/// Maps OpenHarmony `OH_NativeColorSpaceManager` `ColorSpaceName` enum values
/// (`NativeDisplayManager_DisplayInfo::colorSpace`) to the closest built-in ICC profile.
fn ohos_color_space_builtin(color_spaces: &[u32]) -> (photocraft_cms::Builtin, &'static str) {
    use photocraft_cms::Builtin;
    // DCI_P3 = 2, DISPLAY_P3 = 3, P3_HLG = 11, P3_PQ = 12, DISPLAY_P3_LIMIT = 14, LINEAR_P3 = 23
    if color_spaces.iter().any(|&cs| matches!(cs, 2 | 3 | 11 | 12 | 14 | 23)) {
        return (Builtin::DisplayP3, "Display P3");
    }
    // ADOBE_RGB = 1, ADOBE_RGB_LIMIT = 13
    if color_spaces.iter().any(|&cs| matches!(cs, 1 | 13)) {
        return (Builtin::AdobeRgbCompat, "Adobe RGB (1998)");
    }
    // BT2020 = 8, BT2020_HLG = 9, BT2020_PQ = 10, BT2020_LIMIT = 18, BT2020_HLG_LIMIT = 19,
    // BT2020_PQ_LIMIT = 20, LINEAR_BT2020 = 25
    if color_spaces.iter().any(|&cs| matches!(cs, 8 | 9 | 10 | 18 | 19 | 20 | 25)) {
        return (Builtin::Rec2020, "Rec. ITU-R BT.2020");
    }
    (Builtin::Srgb, "sRGB IEC61966-2.1")
}

#[derive(Clone, Copy)]
struct Placement {
    index: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn find_monitor(placements: &[Placement], x: i32, y: i32, w: u32, h: u32) -> Option<usize> {
    placements.iter().find(|p| p.x == x && p.y == y && p.width == w && p.height == h).map(|p| p.index)
}

fn count(width: usize, height: usize) -> Result<usize, String> {
    width.checked_mul(height).filter(|&n| n > 0 && n <= MAX_PIXELS).ok_or_else(|| "Screen capture exceeds the 64 MP memory budget".into())
}

pub fn screen_color_service(cc: &eframe::CreationContext<'_>) -> Option<Service> {
    let window = cc.winit_window()?.clone();
    Some(Box::new(move |ctx| {
        let (tx, rx) = std::sync::mpsc::channel();
        let fallback = tx.clone();
        let placements: Vec<Placement> = window
            .available_monitors()
            .enumerate()
            .map(|(index, m)| {
                let p = m.position();
                let size = m.size();
                Placement { index, x: p.x, y: p.y, width: size.width, height: size.height }
            })
            .collect();
        let ohos_window_id = {
            use winit::platform::ohos::WindowExtOpenHarmony as _;
            window.real_window_id()
        };
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancelled.clone();
        let scale = ctx.native_pixels_per_point().unwrap_or(1.0);
        let wake = ctx.clone();
        if let Err(e) = std::thread::Builder::new().name("screen-color-picker".into()).spawn(move || {
            let result = capture_ohos(ohos_window_id, scale, &stop, &placements);
            let _ = tx.send(result);
            wake.request_repaint();
        }) {
            let _ = fallback.send(Err(format!("Could not start screen color picker: {e}")));
        }
        Pending { receiver: rx, cancelled }
    }))
}

fn capture_ohos(primary_window_id: Option<i32>, scale: f32, stop: &std::sync::atomic::AtomicBool, placements: &[Placement]) -> Result<Capture, String> {
    let mut window_ids = Vec::new();
    if let Some(id) = primary_window_id.filter(|&id| id > 0) {
        window_ids.push(id);
    }
    if let Ok(infos) = openharmony_ability::window_manager::WindowManager::main_windows() {
        for info in infos {
            if info.showing && info.window_id > 0 && !window_ids.contains(&info.window_id) {
                window_ids.push(info.window_id);
            }
        }
    }
    if window_ids.is_empty() {
        return Err("No active OpenHarmony window is available for color picking".into());
    }
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let mut total = 0usize;
    let mut images = Vec::new();
    let mut last_error = None;
    for id in window_ids.into_iter().take(16) {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(Capture::Color(None));
        }
        match openharmony_ability::snapshot_window(id) {
            Ok(snap) => {
                let (width, height) = (snap.width as usize, snap.height as usize);
                total =
                    total.checked_add(count(width, height)?).filter(|&n| n <= MAX_PIXELS).ok_or("Combined windows exceed the screen picker memory budget")?;
                let w_px = if snap.window_width > 0 { snap.window_width } else { snap.width };
                let h_px = if snap.window_height > 0 { snap.window_height } else { snap.height };
                let monitor = find_monitor(placements, snap.window_x, snap.window_y, w_px, h_px).or_else(|| placements.first().map(|p| p.index));
                let profile = profile_for_display(snap.display_id);
                images.push(ScreenImage {
                    monitor,
                    position: egui::pos2(snap.window_x as f32, snap.window_y as f32) / scale,
                    size: egui::vec2(w_px as f32, h_px as f32) / scale,
                    width,
                    height,
                    rgba: snap.rgba,
                    profile,
                });
            }
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
    }
    if images.is_empty() {
        return Err(last_error.unwrap_or_else(|| "OpenHarmony window snapshot failed".into()));
    }
    Ok(Capture::Images(images))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ohos_color_spaces_map_to_valid_icc_profiles() {
        use photocraft_cms::Builtin;
        assert_eq!(ohos_color_space_builtin(&[4, 3]), (Builtin::DisplayP3, "Display P3"));
        assert_eq!(ohos_color_space_builtin(&[1]), (Builtin::AdobeRgbCompat, "Adobe RGB (1998)"));
        assert_eq!(ohos_color_space_builtin(&[9]), (Builtin::Rec2020, "Rec. ITU-R BT.2020"));
        assert_eq!(ohos_color_space_builtin(&[]), (Builtin::Srgb, "sRGB IEC61966-2.1"));
        for cs in [&[3u32][..], &[1u32][..], &[9u32][..], &[][..]] {
            let (builtin, _) = ohos_color_space_builtin(cs);
            let bytes = builtin.profile().to_bytes();
            assert!(photocraft_engine::color_cmds::profile_from_bytes(&bytes).is_ok_and(|p| format!("{:?}", p.color_space) == "Rgb"));
        }
    }
}
