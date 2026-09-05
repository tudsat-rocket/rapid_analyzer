//! The PNG backend: the same figure, rasterized here rather than by a viewer.
//!
//! There is no dependency doing this. Two things make it small enough to do by
//! hand. Shapes are anti-aliased analytically -- a pixel's coverage is its
//! distance to the segment, which for the axis-aligned rectangles and thin
//! polylines a graph is made of is exact enough to be indistinguishable from a
//! sampled rasterizer. And text is not rasterized at all: it is *blitted* from
//! egui's own font atlas, laid out at the export's own resolution
//! ([`super::text`]), so a glyph's atlas entry and its destination are the same
//! size and the letters come out as crisp as they are on screen.
//!
//! Compositing is premultiplied alpha in gamma space -- the same arithmetic
//! egui's shader does, and what SVG renderers do by default, so the two
//! backends produce the same picture.

use std::sync::Arc;

use egui::{Align2, Color32, Galley, Pos2, Rect, Vec2, pos2};

use super::figure::Canvas;
use super::text::TextEngine;

pub struct RasterCanvas {
    /// Figure units.
    size: Vec2,
    /// Device pixels per figure unit.
    scale: f32,
    width: usize,
    height: usize,
    /// Premultiplied RGBA, 0..1, gamma space, row major.
    pixels: Vec<[f32; 4]>,
    /// In device pixels.
    clip: Rect,
    engine: TextEngine,
    /// Text is blitted at the end, once, because the font atlas can only be
    /// read after every glyph in the figure has been laid into it.
    texts: Vec<TextJob>,
}

struct TextJob {
    galley: Arc<Galley>,
    /// Top-left of the text's box, in device pixels.
    origin: Pos2,
    color: Color32,
    /// A quarter turn anticlockwise, for a y-axis label.
    rotated: bool,
    clip: Rect,
}

/// Refuses a canvas so large it would exhaust memory before it drew anything.
///
/// Compositing is in floats -- sixteen bytes a pixel -- so the ceiling is
/// about half a gigabyte of working buffer. 30 megapixels is a 200 x 150 mm
/// figure at 600 dpi, well past what a page can show.
pub const MAX_PIXELS: usize = 30_000_000;

impl RasterCanvas {
    pub fn new(size: Vec2, scale: f32) -> anyhow::Result<Self> {
        let scale = scale.max(0.01);
        let width = (size.x * scale).round().max(1.0) as usize;
        let height = (size.y * scale).round().max(1.0) as usize;
        anyhow::ensure!(
            width.saturating_mul(height) <= MAX_PIXELS,
            "{width}x{height} pixels is too large to render -- reduce the size or the resolution"
        );
        Ok(Self {
            size,
            scale,
            width,
            height,
            pixels: vec![[0.0; 4]; width * height],
            clip: Rect::from_min_size(Pos2::ZERO, Vec2::new(width as f32, height as f32)),
            engine: TextEngine::new(scale),
            texts: Vec::new(),
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Straight (un-premultiplied) RGBA8, ready for a PNG or a texture.
    pub fn into_rgba(mut self) -> (usize, usize, Vec<u8>) {
        self.blit_text();
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for p in &self.pixels {
            let a = p[3].clamp(0.0, 1.0);
            let unmultiply = |v: f32| {
                if a <= 0.0 {
                    0.0
                } else {
                    (v / a).clamp(0.0, 1.0)
                }
            };
            out.push((unmultiply(p[0]) * 255.0).round() as u8);
            out.push((unmultiply(p[1]) * 255.0).round() as u8);
            out.push((unmultiply(p[2]) * 255.0).round() as u8);
            out.push((a * 255.0).round() as u8);
        }
        (self.width, self.height, out)
    }

    /// The figure as an image egui can show, for the export dialog's preview.
    pub fn into_color_image(self) -> egui::ColorImage {
        let (width, height, rgba) = self.into_rgba();
        let pixels = rgba
            .chunks_exact(4)
            .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        egui::ColorImage::new([width, height], pixels)
    }

    /// The figure as a PNG file, with the resolution recorded in it so a word
    /// processor places it at the size it was asked for rather than at
    /// whatever its own default is.
    pub fn into_png(self, dpi: f32) -> anyhow::Result<Vec<u8>> {
        let (width, height, rgba) = self.into_rgba();
        let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
            .ok_or_else(|| anyhow::anyhow!("image buffer does not match {width}x{height}"))?;
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png)?;
        Ok(with_dpi(png.into_inner(), dpi))
    }

    fn blend(&mut self, x: usize, y: usize, color: [f32; 4], coverage: f32) {
        if coverage <= 0.0 {
            return;
        }
        let a = color[3] * coverage;
        if a <= 0.0 {
            return;
        }
        let dst = &mut self.pixels[y * self.width + x];
        let inv = 1.0 - a;
        for i in 0..3 {
            dst[i] = color[i] * coverage + dst[i] * inv;
        }
        dst[3] = a + dst[3] * inv;
    }

    /// The clipped device-pixel row/column range a shape can touch.
    fn bounds(&self, rect: Rect) -> Option<(usize, usize, usize, usize)> {
        let rect = rect.intersect(self.clip);
        let x0 = rect.left().floor().max(0.0) as usize;
        let y0 = rect.top().floor().max(0.0) as usize;
        let x1 = (rect.right().ceil().max(0.0) as usize).min(self.width);
        let y1 = (rect.bottom().ceil().max(0.0) as usize).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    fn fill_device_rect(&mut self, rect: Rect, color: [f32; 4]) {
        let Some((x0, y0, x1, y1)) = self.bounds(rect) else {
            return;
        };
        let clip = self.clip;
        for y in y0..y1 {
            for x in x0..x1 {
                // Coverage is the overlap of the pixel with the rectangle, so
                // an edge landing mid-pixel is a soft edge rather than a jump.
                let cx = overlap(x as f32, rect.left().max(clip.left()), rect.right().min(clip.right()));
                let cy = overlap(y as f32, rect.top().max(clip.top()), rect.bottom().min(clip.bottom()));
                self.blend(x, y, color, cx * cy);
            }
        }
    }

    fn segment(&mut self, a: Pos2, b: Pos2, color: [f32; 4], width: f32) {
        // Below a pixel wide, a line thins by fading rather than by vanishing
        // between two pixel centres.
        let half = width.max(1.0) * 0.5;
        let fade = width.clamp(0.0, 1.0);
        let rect = Rect::from_two_pos(a, b).expand(half + 1.0);
        let Some((x0, y0, x1, y1)) = self.bounds(rect) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let p = pos2(x as f32 + 0.5, y as f32 + 0.5);
                let d = distance_to_segment(p, a, b);
                let coverage = (half + 0.5 - d).clamp(0.0, 1.0) * fade;
                self.blend(x, y, color, coverage);
            }
        }
    }

    /// Draws every glyph of every string, now that the atlas holds them all.
    fn blit_text(&mut self) {
        if self.texts.is_empty() {
            return;
        }
        let atlas = self.engine.atlas();
        let [aw, ah] = atlas.size;
        let scale = self.scale;
        for job in std::mem::take(&mut self.texts) {
            let color = premultiplied(job.color);
            // Turned on its side, the text's *width* is how tall its box is.
            let box_height = if job.rotated {
                job.galley.rect.width()
            } else {
                job.galley.rect.height()
            } * scale;
            for row in &job.galley.rows {
                for glyph in &row.row.glyphs {
                    let uv = glyph.uv_rect;
                    if uv.is_nothing() {
                        continue;
                    }
                    let local = row.pos + glyph.pos.to_vec2() + uv.offset - job.galley.rect.min.to_vec2();
                    let (lx, ly) = ((local.x * scale).round(), (local.y * scale).round());
                    let (sw, sh) = (
                        (uv.max[0] as usize).saturating_sub(uv.min[0] as usize),
                        (uv.max[1] as usize).saturating_sub(uv.min[1] as usize),
                    );
                    for j in 0..sh {
                        for i in 0..sw {
                            let (sx, sy) = (uv.min[0] as usize + i, uv.min[1] as usize + j);
                            if sx >= aw || sy >= ah {
                                continue;
                            }
                            let texel = premultiplied(atlas.pixels[sy * aw + sx]);
                            if texel[3] <= 0.0 {
                                continue;
                            }
                            // The glyph was rasterized at this very scale, so
                            // one texel is one device pixel; no resampling.
                            let (dx, dy) = if job.rotated {
                                (job.origin.x + ly + j as f32, job.origin.y + box_height - lx - i as f32 - 1.0)
                            } else {
                                (job.origin.x + lx + i as f32, job.origin.y + ly + j as f32)
                            };
                            let (dx, dy) = (dx.round(), dy.round());
                            if dx < 0.0 || dy < 0.0 {
                                continue;
                            }
                            let (dx, dy) = (dx as usize, dy as usize);
                            let centre = pos2(dx as f32 + 0.5, dy as f32 + 0.5);
                            if dx >= self.width || dy >= self.height || !job.clip.contains(centre) {
                                continue;
                            }
                            let src = [
                                color[0] * texel[0],
                                color[1] * texel[1],
                                color[2] * texel[2],
                                color[3] * texel[3],
                            ];
                            self.blend(dx, dy, src, 1.0);
                        }
                    }
                }
            }
        }
    }

    fn queue_text(&mut self, origin: Pos2, galley: Arc<Galley>, color: Color32, rotated: bool) {
        self.texts.push(TextJob {
            galley,
            origin,
            color,
            rotated,
            clip: self.clip,
        });
    }
}

impl Canvas for RasterCanvas {
    fn size(&self) -> Vec2 {
        self.size
    }

    fn fill_rect(&mut self, rect: Rect, color: Color32) {
        if color.a() == 0 {
            return;
        }
        let scaled = Rect::from_min_max(
            pos2(rect.left() * self.scale, rect.top() * self.scale),
            pos2(rect.right() * self.scale, rect.bottom() * self.scale),
        );
        self.fill_device_rect(scaled, premultiplied(color));
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color32, width: f32) {
        let c = [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
            rect.left_top(),
        ];
        self.polyline(&c, color, width);
    }

    fn polyline(&mut self, points: &[Pos2], color: Color32, width: f32) {
        if points.len() < 2 || color.a() == 0 || width <= 0.0 {
            return;
        }
        let color = premultiplied(color);
        let width = width * self.scale;
        for pair in points.windows(2) {
            let (a, b) = (
                (pair[0].to_vec2() * self.scale).to_pos2(),
                (pair[1].to_vec2() * self.scale).to_pos2(),
            );
            if !a.x.is_finite() || !a.y.is_finite() || !b.x.is_finite() || !b.y.is_finite() {
                continue;
            }
            self.segment(a, b, color, width);
        }
    }

    fn text(&mut self, anchor: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        if text.is_empty() || color.a() == 0 {
            return;
        }
        let galley = self.engine.layout(text, size, color);
        let rect = align.anchor_size(anchor, galley.rect.size());
        let origin = pos2(rect.left() * self.scale, rect.top() * self.scale);
        self.queue_text(origin, galley, color, false);
    }

    fn text_rotated(&mut self, center: Pos2, text: &str, size: f32, color: Color32) {
        if text.is_empty() || color.a() == 0 {
            return;
        }
        let galley = self.engine.layout(text, size, color);
        // Turned a quarter turn, the text's width becomes the box's height.
        let size = Vec2::new(galley.rect.height(), galley.rect.width());
        let rect = Align2::CENTER_CENTER.anchor_size(center, size);
        let origin = pos2(rect.left() * self.scale, rect.top() * self.scale);
        self.queue_text(origin, galley, color, true);
    }

    fn measure(&mut self, text: &str, size: f32) -> Vec2 {
        self.engine.measure(text, size)
    }

    fn clip(&mut self, rect: Option<Rect>) {
        self.clip = match rect {
            Some(rect) => Rect::from_min_max(
                pos2(rect.left() * self.scale, rect.top() * self.scale),
                pos2(rect.right() * self.scale, rect.bottom() * self.scale),
            )
            .intersect(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            None => Rect::from_min_size(Pos2::ZERO, Vec2::new(self.width as f32, self.height as f32)),
        };
    }
}

/// A [`Color32`] (premultiplied, gamma space) as floats.
fn premultiplied(color: Color32) -> [f32; 4] {
    let [r, g, b, a] = color.to_array();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}

/// How much of the pixel starting at `x` lies between `lo` and `hi`.
fn overlap(x: f32, lo: f32, hi: f32) -> f32 {
    (hi.min(x + 1.0) - lo.max(x)).clamp(0.0, 1.0)
}

fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Inserts a `pHYs` chunk (the PNG's physical pixel size) after the header.
///
/// Without it a 300 dpi figure is a very large picture rather than a small
/// sharp one: a word processor has nothing to scale it by but its own guess.
fn with_dpi(png: Vec<u8>, dpi: f32) -> Vec<u8> {
    const HEADER_END: usize = 8 + 8 + 13 + 4; // signature + IHDR (length, type, data, CRC)
    if png.len() < HEADER_END || &png[12..16] != b"IHDR" || !dpi.is_finite() || dpi <= 0.0 {
        return png;
    }
    let per_metre = (dpi as f64 * 39.370_078_74).round().clamp(1.0, u32::MAX as f64) as u32;
    let mut chunk = Vec::with_capacity(21);
    chunk.extend_from_slice(&9u32.to_be_bytes());
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(b"pHYs");
    body.extend_from_slice(&per_metre.to_be_bytes());
    body.extend_from_slice(&per_metre.to_be_bytes());
    body.push(1); // unit: metres
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());

    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..HEADER_END]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[HEADER_END..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * width + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn the_canvas_is_the_figure_at_the_requested_scale() {
        let canvas = RasterCanvas::new(Vec2::new(200.0, 100.0), 3.0).unwrap();
        assert_eq!((canvas.width(), canvas.height()), (600, 300));
    }

    #[test]
    fn an_absurd_resolution_is_refused_rather_than_allocated() {
        let Err(err) = RasterCanvas::new(Vec2::new(4000.0, 3000.0), 100.0) else {
            panic!("a canvas of 120 gigapixels was allocated");
        };
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[test]
    fn nothing_drawn_leaves_the_canvas_transparent() {
        let (w, _, rgba) = RasterCanvas::new(Vec2::new(4.0, 4.0), 1.0).unwrap().into_rgba();
        assert_eq!(pixel(&rgba, w, 2, 2)[3], 0);
    }

    #[test]
    fn a_filled_rect_covers_exactly_what_it_says() {
        let mut canvas = RasterCanvas::new(Vec2::new(10.0, 10.0), 1.0).unwrap();
        canvas.fill_rect(Rect::from_min_max(pos2(2.0, 2.0), pos2(5.0, 5.0)), Color32::RED);
        let (w, _, rgba) = canvas.into_rgba();
        assert_eq!(pixel(&rgba, w, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, w, 1, 3)[3], 0, "outside the rect");
        assert_eq!(pixel(&rgba, w, 5, 3)[3], 0, "the far edge is exclusive");
    }

    #[test]
    fn a_line_is_drawn_where_it_was_asked_for_and_is_anti_aliased() {
        let mut canvas = RasterCanvas::new(Vec2::new(20.0, 20.0), 1.0).unwrap();
        // Three units wide about y=10: two whole pixels either side of the
        // boundary, and half a pixel of coverage beyond each.
        canvas.polyline(&[pos2(2.0, 10.0), pos2(18.0, 10.0)], Color32::BLACK, 3.0);
        let (w, _, rgba) = canvas.into_rgba();
        assert!(pixel(&rgba, w, 10, 9)[3] > 250, "the line itself");
        assert!(pixel(&rgba, w, 10, 10)[3] > 250, "the line itself");
        let edge = pixel(&rgba, w, 10, 8)[3];
        assert!(edge > 20 && edge < 235, "a soft edge, got {edge}");
        assert_eq!(pixel(&rgba, w, 10, 15)[3], 0, "well clear of the line");
        // The ends are rounded, as they are in the SVG the other backend
        // writes -- so the cap reaches past x=2, but not by a whole pixel.
        assert!(pixel(&rgba, w, 0, 10)[3] < pixel(&rgba, w, 3, 10)[3]);
        assert_eq!(pixel(&rgba, w, 0, 13)[3], 0, "clear of the cap");
    }

    #[test]
    fn clipping_keeps_a_line_inside_the_plot() {
        let mut canvas = RasterCanvas::new(Vec2::new(20.0, 20.0), 1.0).unwrap();
        canvas.clip(Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 20.0))));
        canvas.polyline(&[pos2(2.0, 10.0), pos2(18.0, 10.0)], Color32::BLACK, 2.0);
        canvas.clip(None);
        let (w, _, rgba) = canvas.into_rgba();
        assert!(pixel(&rgba, w, 5, 10)[3] > 200, "inside the clip");
        assert_eq!(pixel(&rgba, w, 15, 10)[3], 0, "outside the clip");
    }

    #[test]
    fn text_puts_ink_on_the_canvas_where_it_was_anchored() {
        let mut canvas = RasterCanvas::new(Vec2::new(60.0, 20.0), 2.0).unwrap();
        canvas.fill_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(60.0, 20.0)), Color32::WHITE);
        canvas.text(pos2(30.0, 10.0), Align2::CENTER_CENTER, "42.0", 10.0, Color32::BLACK);
        let (w, h, rgba) = canvas.into_rgba();
        let dark = (0..w * h)
            .filter(|i| rgba[i * 4] < 128 && rgba[i * 4 + 3] > 128)
            .count();
        assert!(dark > 20, "expected glyph pixels, found {dark}");
        // ... in the middle, not at the edges.
        assert_eq!(pixel(&rgba, w, 1, 1), [255, 255, 255, 255]);
    }

    #[test]
    fn a_rotated_label_lands_in_its_own_box() {
        let mut canvas = RasterCanvas::new(Vec2::new(20.0, 60.0), 2.0).unwrap();
        canvas.text_rotated(pos2(10.0, 30.0), "bar", 10.0, Color32::BLACK);
        let (w, h, rgba) = canvas.into_rgba();
        let ink: Vec<(usize, usize)> = (0..w * h)
            .filter(|i| rgba[i * 4 + 3] > 40)
            .map(|i| (i % w, i / w))
            .collect();
        assert!(!ink.is_empty(), "the label drew nothing");
        let tall = ink.iter().map(|p| p.1).max().unwrap() - ink.iter().map(|p| p.1).min().unwrap();
        let wide = ink.iter().map(|p| p.0).max().unwrap() - ink.iter().map(|p| p.0).min().unwrap();
        assert!(tall > wide, "a rotated label reads down the page ({tall} vs {wide})");
    }

    #[test]
    fn the_png_records_the_resolution_it_was_rendered_at() {
        let mut canvas = RasterCanvas::new(Vec2::new(10.0, 10.0), 2.0).unwrap();
        canvas.fill_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0)), Color32::WHITE);
        let png = canvas.into_png(300.0).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let phys = png.windows(4).position(|w| w == b"pHYs").expect("a pHYs chunk");
        let ppm = u32::from_be_bytes(png[phys + 4..phys + 8].try_into().unwrap());
        assert_eq!(ppm, 11811, "300 dpi in pixels per metre");
        // The image still decodes, with the chunk in it.
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (20, 20));
    }

    #[test]
    fn the_crc_matches_the_reference_implementation() {
        // The check value every CRC-32 implementation agrees on.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
