//! The SVG backend: a figure as vector text, which is what a report wants --
//! it stays sharp at any zoom and prints at the printer's resolution rather
//! than the file's.
//!
//! The one thing an SVG cannot carry here is the font. Text is emitted as
//! `<text>` for the viewer to draw in whatever sans-serif it has, which keeps
//! the file small and the labels selectable, at the cost of glyph widths we
//! cannot predict exactly. The layout therefore leaves every string a little
//! more room than it measured (`figure::TEXT_SLACK`) and *anchors* it --
//! start, middle or end -- so a viewer whose font is a little wider or
//! narrower moves the text within the room reserved for it instead of walking
//! out of the figure.

use egui::{Align, Align2, Color32, Pos2, Rect, Vec2};

use super::figure::Canvas;
use super::text::TextEngine;

/// Ubuntu first, because that is what the app itself lays out with; the rest
/// is the usual portable fallback chain.
const FONT_STACK: &str = "Ubuntu, 'DejaVu Sans', Helvetica, Arial, sans-serif";

pub struct SvgCanvas {
    size: Vec2,
    /// Physical size the figure is meant to be printed at.
    size_mm: Vec2,
    engine: TextEngine,
    defs: String,
    body: String,
    clips: usize,
    clipped: bool,
    gradients: usize,
}

impl SvgCanvas {
    pub fn new(size: Vec2, size_mm: Vec2) -> Self {
        Self {
            size,
            size_mm,
            // Only ever used for measurement, so the scale is irrelevant.
            engine: TextEngine::new(1.0),
            defs: String::new(),
            body: String::new(),
            clips: 0,
            clipped: false,
            gradients: 0,
        }
    }

    /// The finished document.
    pub fn finish(mut self) -> String {
        self.clip(None);
        let mut out = String::with_capacity(self.body.len() + 512);
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
        out.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"{}mm\" height=\"{}mm\" \
             viewBox=\"0 0 {} {}\">\n",
            num(self.size_mm.x),
            num(self.size_mm.y),
            num(self.size.x),
            num(self.size.y)
        ));
        out.push_str("<!-- exported by rapid-analyzer -->\n");
        if !self.defs.is_empty() {
            out.push_str("<defs>\n");
            out.push_str(&self.defs);
            out.push_str("</defs>\n");
        }
        out.push_str(&self.body);
        out.push_str("</svg>\n");
        out
    }
}

impl Canvas for SvgCanvas {
    fn size(&self) -> Vec2 {
        self.size
    }

    fn fill_rect(&mut self, rect: Rect, color: Color32) {
        if color.a() == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let (fill, opacity) = paint(color);
        self.body.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\"{opacity}/>\n",
            num(rect.left()),
            num(rect.top()),
            num(rect.width()),
            num(rect.height())
        ));
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color32, width: f32) {
        if color.a() == 0 || width <= 0.0 {
            return;
        }
        let (stroke, opacity) = paint(color);
        let opacity = opacity.replace("fill-opacity", "stroke-opacity");
        self.body.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"none\" stroke=\"{stroke}\" \
             stroke-width=\"{}\"{opacity}/>\n",
            num(rect.left()),
            num(rect.top()),
            num(rect.width()),
            num(rect.height()),
            num(width)
        ));
    }

    fn polyline(&mut self, points: &[Pos2], color: Color32, width: f32) {
        if points.len() < 2 || color.a() == 0 || width <= 0.0 {
            return;
        }
        let (stroke, opacity) = paint(color);
        let opacity = opacity.replace("fill-opacity", "stroke-opacity");
        let mut d = String::with_capacity(points.len() * 12);
        for (i, p) in points.iter().enumerate() {
            if i > 0 {
                d.push(' ');
            }
            d.push_str(&num(p.x));
            d.push(',');
            d.push_str(&num(p.y));
        }
        self.body.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{}\" stroke-linecap=\"round\" \
             stroke-linejoin=\"round\"{opacity} points=\"{d}\"/>\n",
            num(width)
        ));
    }

    fn fill_cell(&mut self, rect: Rect, color: Color32) {
        if color.a() == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let (fill, opacity) = paint(color);
        let cell = cell_geometry(rect);
        self.body.push_str(&format!(
            "<rect {cell} fill=\"{fill}\"{opacity} shape-rendering=\"crispEdges\"/>\n"
        ));
    }

    fn vertical_gradient(&mut self, rect: Rect, stops: &[(f32, Color32)]) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 || stops.is_empty() {
            return;
        }
        // One stop is a flat fill; a gradient with a single stop is not
        // defined the same way by every renderer.
        if stops.len() == 1 {
            self.fill_cell(rect, stops[0].1);
            return;
        }
        let id = format!("grad{}", self.gradients);
        self.gradients += 1;
        // Object bounding box units, so the same gradient definition works
        // whatever size the rect is.
        self.defs.push_str(&format!(
            "<linearGradient id=\"{id}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">"
        ));
        for (at, color) in stops {
            let [r, g, b, a] = color.to_srgba_unmultiplied();
            let opacity = if a == 255 {
                String::new()
            } else {
                format!(" stop-opacity=\"{}\"", num(a as f32 / 255.0))
            };
            self.defs.push_str(&format!(
                "<stop offset=\"{}\" stop-color=\"#{r:02x}{g:02x}{b:02x}\"{opacity}/>",
                num(at.clamp(0.0, 1.0))
            ));
        }
        self.defs.push_str("</linearGradient>\n");
        // Crisp edges: two of these side by side must not leave a hairline of
        // background between them -- see `Canvas::fill_cell`.
        let cell = cell_geometry(rect);
        self.body.push_str(&format!(
            "<rect {cell} fill=\"url(#{id})\" shape-rendering=\"crispEdges\"/>\n"
        ));
    }

    fn text(&mut self, anchor: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        if text.is_empty() || color.a() == 0 {
            return;
        }
        let height = self.engine.line_height(size);
        let ascent = self.engine.ascent(size);
        // SVG puts text on its baseline; `dominant-baseline` is not reliable
        // across viewers, so the offset is worked out here.
        let baseline = match align.y() {
            Align::Min => anchor.y + ascent,
            Align::Center => anchor.y + ascent - height * 0.5,
            Align::Max => anchor.y + ascent - height,
        };
        let (fill, opacity) = paint(color);
        self.body.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-family=\"{FONT_STACK}\" font-size=\"{}\" fill=\"{fill}\"{opacity} \
             text-anchor=\"{}\">{}</text>\n",
            num(anchor.x),
            num(baseline),
            num(size),
            anchor_for(align.x()),
            escape(text)
        ));
    }

    fn text_rotated(&mut self, center: Pos2, text: &str, size: f32, color: Color32) {
        if text.is_empty() || color.a() == 0 {
            return;
        }
        let height = self.engine.line_height(size);
        let ascent = self.engine.ascent(size);
        let (fill, opacity) = paint(color);
        // Rotating about the anchor keeps the baseline shift with it, so the
        // label ends up centred on `center` whichever way round it reads.
        self.body.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" transform=\"rotate(-90 {x} {cy})\" font-family=\"{FONT_STACK}\" \
             font-size=\"{}\" fill=\"{fill}\"{opacity} text-anchor=\"middle\">{}</text>\n",
            num(size),
            escape(text),
            x = num(center.x),
            y = num(center.y + ascent - height * 0.5),
            cy = num(center.y),
        ));
    }

    fn measure(&mut self, text: &str, size: f32) -> Vec2 {
        // Deliberately the same number the raster backend reports: the two
        // must lay a figure out identically, or the preview would not be of
        // the file. Room for a wider font is added by the layout itself, for
        // both backends alike -- see `figure::TEXT_SLACK`.
        self.engine.measure(text, size)
    }

    fn clip(&mut self, rect: Option<Rect>) {
        if self.clipped {
            self.body.push_str("</g>\n");
            self.clipped = false;
        }
        let Some(rect) = rect else {
            return;
        };
        let id = format!("clip{}", self.clips);
        self.clips += 1;
        self.defs.push_str(&format!(
            "<clipPath id=\"{id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>\n",
            num(rect.left()),
            num(rect.top()),
            num(rect.width().max(0.0)),
            num(rect.height().max(0.0))
        ));
        self.body.push_str(&format!("<g clip-path=\"url(#{id})\">\n"));
        self.clipped = true;
    }
}

/// `#rrggbb`, plus an opacity attribute when the colour is not opaque.
fn paint(color: Color32) -> (String, String) {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let hex = format!("#{r:02x}{g:02x}{b:02x}");
    let opacity = if a == 255 {
        String::new()
    } else {
        format!(" fill-opacity=\"{}\"", num(a as f32 / 255.0))
    };
    (hex, opacity)
}

fn anchor_for(align: Align) -> &'static str {
    match align {
        Align::Min => "start",
        Align::Center => "middle",
        Align::Max => "end",
    }
}

/// A cell's `x`/`y`/`width`/`height`, arrived at from its *edges*.
///
/// Both edges are rounded to the grid the numbers are printed on, and the size
/// is their difference -- so the right edge of one cell is written as exactly
/// the same number as the left edge of the next. Taking the width from the
/// rect instead lets the two disagree in the last decimal, and a renderer
/// snapping each cell to the pixel grid separately then leaves a one-pixel
/// hairline of background between them.
fn cell_geometry(rect: Rect) -> String {
    let (left, top) = (quantize(rect.left()), quantize(rect.top()));
    let (right, bottom) = (quantize(rect.right()), quantize(rect.bottom()));
    format!(
        "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
        num(left),
        num(top),
        num((right - left).max(0.0)),
        num((bottom - top).max(0.0))
    )
}

/// A coordinate on the grid [`num`] prints on.
fn quantize(v: f32) -> f32 {
    if v.is_finite() { (v * 100.0).round() / 100.0 } else { 0.0 }
}

/// Two decimals is a fiftieth of a pixel -- below anything a renderer can
/// show, and it keeps a 2000-point series to a readable file size.
fn num(v: f32) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{v:.2}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" { "0".to_string() } else { s }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> SvgCanvas {
        SvgCanvas::new(Vec2::new(400.0, 300.0), Vec2::new(105.83, 79.38))
    }

    #[test]
    fn the_document_carries_a_physical_size_and_a_viewbox() {
        let svg = canvas().finish();
        assert!(svg.starts_with("<?xml"), "{}", &svg[..40]);
        assert!(svg.contains("width=\"105.83mm\""), "{svg}");
        assert!(svg.contains("viewBox=\"0 0 400 300\""), "{svg}");
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn a_clip_group_is_opened_and_always_closed() {
        let mut c = canvas();
        c.clip(Some(Rect::from_min_max(Pos2::new(1.0, 2.0), Pos2::new(3.0, 4.0))));
        c.polyline(&[Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)], Color32::RED, 1.0);
        let svg = c.finish();
        assert_eq!(svg.matches("<g clip-path").count(), 1, "{svg}");
        assert_eq!(svg.matches("</g>").count(), 1, "{svg}");
        assert!(svg.contains("<clipPath id=\"clip0\">"), "{svg}");
    }

    #[test]
    fn markup_in_a_series_name_cannot_break_the_document() {
        let mut c = canvas();
        c.text(
            Pos2::new(10.0, 10.0),
            Align2::LEFT_TOP,
            "a < b & \"c\"",
            10.0,
            Color32::BLACK,
        );
        let svg = c.finish();
        assert!(svg.contains("a &lt; b &amp; &quot;c&quot;"), "{svg}");
        assert!(!svg.contains("a < b"), "{svg}");
    }

    #[test]
    fn a_translucent_colour_becomes_an_opacity() {
        let (hex, opacity) = paint(Color32::from_rgb(0x2E, 0x7E, 0xE6));
        assert_eq!(hex, "#2e7ee6");
        assert!(opacity.is_empty());
        // Color32 keeps colours premultiplied, so a translucent one comes
        // back a shade off what went in; what matters is the opacity.
        let (hex, opacity) = paint(Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 128));
        assert_eq!(hex.len(), 7, "{hex}");
        assert!(opacity.contains("0.5"), "{opacity}");
    }

    /// The seam this exists to prevent: two cells side by side, printed so
    /// that one's right edge is the other's left edge to the last decimal.
    #[test]
    fn tiled_cells_share_their_edge_exactly() {
        let first = Rect::from_min_max(Pos2::new(10.004, 0.0), Pos2::new(11.337, 5.0));
        let second = Rect::from_min_max(Pos2::new(11.337, 0.0), Pos2::new(12.671, 5.0));
        let (a, b) = (cell_geometry(first), cell_geometry(second));
        let right_of_a = quantize(first.left()) + (quantize(first.right()) - quantize(first.left()));
        assert_eq!(num(right_of_a), num(quantize(second.left())));
        assert!(a.contains("x=\"10\"") && a.contains("width=\"1.34\""), "{a}");
        assert!(b.contains("x=\"11.34\""), "{b}");
    }

    #[test]
    fn numbers_stay_short() {
        assert_eq!(num(3.0), "3");
        assert_eq!(num(3.14259), "3.14");
        assert_eq!(num(-0.001), "0");
        assert_eq!(num(f32::NAN), "0");
    }
}
