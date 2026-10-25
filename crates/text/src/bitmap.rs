//! Embedded colour glyphs (CBDT/CBLC and sbix), composed in the document's pixel format.
//!
//! Font bitmaps use sRGB pixels. As with outline style colours, conversion to the document
//! colour model happens before compositing; alpha interpolation is always premultiplied.

use std::collections::HashMap;
use std::io::Cursor;

use photocraft_color::{Color, PixelFormat};
use photocraft_geom::Affine;
use skrifa::bitmap::{BitmapData, BitmapGlyph, Origin};
use skrifa::instance::{LocationRef, Size};
use skrifa::{GlyphId, MetadataProvider};

use crate::layout::{PlacedGlyph, TextLayout};
use crate::raster::{Bounds, Xform};
use crate::render::{color_in, glyph_xform};
use crate::warp::Warp;

const MAX_SIDE: u32 = 2048;
const MAX_BYTES: usize = 16 * 1024 * 1024;

fn byte_len(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    (width as usize).checked_mul(height as usize)?.checked_mul(4).filter(|&n| n <= MAX_BYTES)
}

fn glyph<'a>(layout: &'a TextLayout, placed: &PlacedGlyph) -> Option<(BitmapGlyph<'a>, Xform)> {
    let face = layout.faces.get(placed.face as usize)?;
    layout.styles.get(placed.style as usize)?;
    let font = skrifa::FontRef::from_index(face.font.data.as_ref(), face.font.index).ok()?;
    let bitmap = font.bitmap_strikes().glyph_for_size(Size::new(face.size_px), GlyphId::new(placed.id))?;
    if !matches!(bitmap.data, BitmapData::Png(_) | BitmapData::Bgra(_)) {
        return None;
    }
    byte_len(bitmap.width, bitmap.height)?;
    let upem = font.metrics(Size::unscaled(), LocationRef::default()).units_per_em;
    if upem == 0 || bitmap.ppem_x <= 0.0 || bitmap.ppem_y <= 0.0 || face.size_px <= 0.0 {
        return None;
    }
    let sx = f64::from(face.size_px / bitmap.ppem_x);
    let sy = f64::from(face.size_px / bitmap.ppem_y);
    let font_scale = f64::from(face.size_px) / f64::from(upem);
    let x = f64::from(bitmap.bearing_x) * font_scale + f64::from(bitmap.inner_bearing_x) * sx;
    let mut y = f64::from(bitmap.bearing_y) * font_scale + f64::from(bitmap.inner_bearing_y) * sy;
    if bitmap.placement_origin == Origin::BottomLeft {
        y += f64::from(bitmap.height) * sy;
    }
    let transform = glyph_xform(layout, placed, face.skew_deg).mul(&Xform([sx, 0.0, 0.0, -sy, x, y]));
    transform.0.iter().all(|v| v.is_finite()).then_some((bitmap, transform))
}

/// A regular grid in bitmap coordinates makes the same warped geometry serve bounds and paint.
fn mesh(width: u32, height: u32, local: Xform, post: Xform, warp: Option<&Warp>, mut triangle: impl FnMut([Vertex; 3])) {
    let steps = |extent: u32| warp.map_or(1, |w| ((f64::from(extent) * local.max_scale() / w.max_segment()).ceil() as usize).clamp(1, 64));
    let (nx, ny) = (steps(width), steps(height));
    let vertex = |x: usize, y: usize| {
        let uv = (x as f64 * f64::from(width) / nx as f64, y as f64 * f64::from(height) / ny as f64);
        let p = local.apply(uv.0, uv.1);
        let p = warp.map_or(p, |w| w.apply(p.0, p.1));
        Vertex { p: post.apply(p.0, p.1), uv }
    };
    for y in 0..ny {
        for x in 0..nx {
            let (a, b, c, d) = (vertex(x, y), vertex(x + 1, y), vertex(x + 1, y + 1), vertex(x, y + 1));
            triangle([a, b, c]);
            triangle([a, c, d]);
        }
    }
}

#[derive(Clone, Copy)]
struct Vertex {
    p: (f64, f64),
    uv: (f64, f64),
}

pub(crate) fn has_visible(layout: &TextLayout) -> bool {
    layout.glyphs.iter().any(|placed| {
        layout.styles.get(placed.style as usize).is_some_and(|s| s.color.alpha.is_finite() && s.color.alpha > 0.0) && glyph(layout, placed).is_some()
    })
}

pub(crate) fn bounds(layout: &TextLayout, transform: &Affine, warp: Option<&Warp>, bounds: &mut Bounds) {
    for placed in &layout.glyphs {
        if let Some((bitmap, local)) = glyph(layout, placed) {
            mesh(bitmap.width, bitmap.height, local, Xform(transform.m), warp, |vertices| {
                for v in vertices {
                    bounds.add(v.p);
                }
            });
        }
    }
}

/// Decode into straight RGBA. Invalid or oversized embedded images are ignored, never allocated
/// according to an untrusted PNG header before dimensions and decoder limits have been checked.
fn decode(bitmap: &BitmapGlyph<'_>) -> Option<Vec<u8>> {
    let len = byte_len(bitmap.width, bitmap.height)?;
    match bitmap.data {
        BitmapData::Bgra(data) => {
            let mut out = data.get(..len)?.to_vec();
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
                let alpha = u32::from(px[3]);
                for c in &mut px[..3] {
                    *c = (u32::from(*c) * 255 + alpha / 2).checked_div(alpha).unwrap_or(0).min(255) as u8;
                }
            }
            Some(out)
        }
        BitmapData::Png(data) => {
            if data.len() > MAX_BYTES {
                return None;
            }
            let mut decoder = png::Decoder::new_with_limits(Cursor::new(data), png::Limits { bytes: MAX_BYTES });
            decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
            let mut reader = decoder.read_info().ok()?;
            if reader.info().width != bitmap.width || reader.info().height != bitmap.height {
                return None;
            }
            let size = reader.output_buffer_size().filter(|&n| n <= MAX_BYTES)?;
            let mut bytes = vec![0; size];
            let info = reader.next_frame(&mut bytes).ok()?;
            let data = bytes.get(..info.buffer_size())?;
            let mut rgba = Vec::with_capacity(len);
            match info.color_type {
                png::ColorType::Rgba => rgba.extend_from_slice(data),
                png::ColorType::Rgb => {
                    for p in data.chunks_exact(3) {
                        rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
                    }
                }
                png::ColorType::GrayscaleAlpha => {
                    for p in data.chunks_exact(2) {
                        rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
                    }
                }
                png::ColorType::Grayscale => {
                    for &v in data {
                        rgba.extend_from_slice(&[v, v, v, 255]);
                    }
                }
                png::ColorType::Indexed => return None,
            }
            (rgba.len() == len).then_some(rgba)
        }
        _ => None,
    }
}

struct Image {
    width: usize,
    height: usize,
    pixels: Vec<f32>,
    stride: usize,
}

impl Image {
    fn new(bitmap: &BitmapGlyph<'_>, format: &PixelFormat) -> Option<Self> {
        let rgba = decode(bitmap)?;
        let stride = format.mode.color_channels() + 1;
        let mut pixels = Vec::with_capacity(rgba.len() / 4 * stride);
        for p in rgba.chunks_exact(4) {
            let alpha = f32::from(p[3]) / 255.0;
            let color = Color::rgba(f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0, alpha);
            pixels.extend(color_in(format, &color).into_iter().map(|c| c * alpha));
            pixels.push(alpha);
        }
        Some(Self { width: bitmap.width as usize, height: bitmap.height as usize, pixels, stride })
    }

    fn sample(&self, u: f64, v: f64, out: &mut [f32; 5]) {
        let x = (u - 0.5).clamp(0.0, self.width.saturating_sub(1) as f64);
        let y = (v - 0.5).clamp(0.0, self.height.saturating_sub(1) as f64);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = ((x - x.floor()) as f32, (y - y.floor()) as f32);
        for (dx, dy, weight) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
            let (x, y) = ((x0 + dx).min(self.width - 1), (y0 + dy).min(self.height - 1));
            let start = (y * self.width + x) * self.stride;
            if let Some(pixel) = self.pixels.get(start..start + self.stride) {
                for (dest, source) in out.iter_mut().zip(pixel) {
                    *dest += source * weight;
                }
            }
        }
    }
}

fn edge(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

fn paint_triangle(mut vertices: [Vertex; 3], image: &Image, opacity: f32, width: usize, height: usize, acc: &mut [f32]) {
    if !vertices.iter().all(|v| v.p.0.is_finite() && v.p.1.is_finite()) {
        return;
    }
    let area = edge(vertices[0].p, vertices[1].p, vertices[2].p);
    if area.abs() < 1e-10 {
        return;
    }
    if area < 0.0 {
        vertices.swap(1, 2);
    }
    let [a, b, c] = vertices;
    let area = area.abs();
    let min_x = a.p.0.min(b.p.0).min(c.p.0).floor().clamp(0.0, width as f64) as usize;
    let max_x = a.p.0.max(b.p.0).max(c.p.0).ceil().clamp(0.0, width as f64) as usize;
    let min_y = a.p.1.min(b.p.1).min(c.p.1).floor().clamp(0.0, height as f64) as usize;
    let max_y = a.p.1.max(b.p.1).max(c.p.1).ceil().clamp(0.0, height as f64) as usize;
    // Opposite ownership of shared edges prevents double blending along each mesh diagonal.
    let inside = |e: f64, a: Vertex, b: Vertex| e > 0.0 || (e == 0.0 && (b.p.1 < a.p.1 || (b.p.1 == a.p.1 && b.p.0 > a.p.0)));
    for y in min_y..max_y {
        for x in min_x..max_x {
            let p = (x as f64 + 0.5, y as f64 + 0.5);
            let (ea, eb, ec) = (edge(b.p, c.p, p), edge(c.p, a.p, p), edge(a.p, b.p, p));
            if !inside(ea, b, c) || !inside(eb, c, a) || !inside(ec, a, b) {
                continue;
            }
            let u = (ea * a.uv.0 + eb * b.uv.0 + ec * c.uv.0) / area;
            let v = (ea * a.uv.1 + eb * b.uv.1 + ec * c.uv.1) / area;
            let mut sample = [0.0; 5];
            image.sample(u, v, &mut sample);
            let alpha = sample.get(image.stride - 1).copied().unwrap_or(0.0) * opacity;
            let start = (y * width + x) * image.stride;
            if let Some(dest) = acc.get_mut(start..start + image.stride) {
                for (dest, source) in dest.iter_mut().zip(sample) {
                    *dest = source * opacity + *dest * (1.0 - alpha);
                }
            }
        }
    }
}

pub(crate) fn composite(layout: &TextLayout, transform: &Xform, warp: Option<&Warp>, format: &PixelFormat, width: usize, height: usize, acc: &mut [f32]) {
    let mut images: HashMap<(u32, u32), Option<Image>> = HashMap::new();
    let mut cached_bytes = 0usize;
    for placed in &layout.glyphs {
        let Some((bitmap, local)) = glyph(layout, placed) else { continue };
        let Some(style) = layout.styles.get(placed.style as usize) else { continue };
        let key = (placed.face, placed.id);
        if !images.contains_key(&key) {
            // Keep repeated letters cheap without retaining an image for every glyph in a
            // hostile document. One decoded image is separately bounded by MAX_SIDE.
            let image = Image::new(&bitmap, format);
            let bytes = image.as_ref().map_or(0, |image| image.pixels.len() * size_of::<f32>());
            if cached_bytes.saturating_add(bytes) > 32 * 1024 * 1024 || images.len() >= 256 {
                images.clear();
                cached_bytes = 0;
            }
            cached_bytes += bytes;
            images.insert(key, image);
        }
        let Some(image) = images.get(&key) else { continue };
        let Some(image) = image else { continue };
        let opacity = if style.color.alpha.is_finite() { style.color.alpha.clamp(0.0, 1.0) } else { 0.0 };
        mesh(bitmap.width, bitmap.height, local, *transform, warp, |vertices| paint_triangle(vertices, image, opacity, width, height, acc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(data: BitmapData<'_>) -> BitmapGlyph<'_> {
        BitmapGlyph {
            data,
            bearing_x: 0.0,
            bearing_y: 0.0,
            inner_bearing_x: 0.0,
            inner_bearing_y: 0.0,
            ppem_x: 2.0,
            ppem_y: 2.0,
            advance: None,
            width: 2,
            height: 2,
            placement_origin: Origin::TopLeft,
        }
    }

    #[test]
    fn bgra_alpha_is_unpremultiplied_and_bad_lengths_are_rejected() {
        let pixels = [0, 0, 128, 128].repeat(4);
        assert_eq!(decode(&bitmap(BitmapData::Bgra(&pixels))), Some([255, 0, 0, 128].repeat(4)));
        assert!(decode(&bitmap(BitmapData::Bgra(&pixels[..8]))).is_none());
        assert!(decode(&bitmap(BitmapData::Png(b"not a PNG"))).is_none());
        let mut huge = bitmap(BitmapData::Bgra(&pixels));
        huge.width = u32::MAX;
        assert!(decode(&huge).is_none());
    }

    #[test]
    fn png_dimensions_must_match_font_metrics() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&[255, 32, 0, 128].repeat(4)).unwrap();
        }
        assert_eq!(decode(&bitmap(BitmapData::Png(&bytes))), Some([255, 32, 0, 128].repeat(4)));
        let mut wrong = bitmap(BitmapData::Png(&bytes));
        wrong.width = 1;
        assert!(decode(&wrong).is_none());
    }

    #[test]
    fn transformed_colour_bitmap_keeps_alpha_across_mesh_seams_and_pixel_formats() {
        let pixels = [0, 0, 128, 128].repeat(4);
        for format in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAYA8, PixelFormat::CMYKA8] {
            let image = Image::new(&bitmap(BitmapData::Bgra(&pixels)), &format).unwrap();
            let mut acc = vec![0.0; 36 * image.stride];
            // Rotate 90 degrees, double the size and translate: covers [1,5) × [1,5).
            mesh(2, 2, Xform([0.0, 2.0, -2.0, 0.0, 5.0, 1.0]), Xform::IDENTITY, None, |v| paint_triangle(v, &image, 0.5, 6, 6, &mut acc));
            for y in 0..6 {
                for x in 0..6 {
                    let alpha = acc[(y * 6 + x) * image.stride + image.stride - 1];
                    let expected = if (1..5).contains(&x) && (1..5).contains(&y) { 128.0 / 255.0 * 0.5 } else { 0.0 };
                    assert!((alpha - expected).abs() < 1e-6, "{format:?} at {x},{y}: {alpha}");
                }
            }
        }
    }
}
