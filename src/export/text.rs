//! Text for the exporter: one font stack, laid out once, used by both backends.
//!
//! The two backends need the same numbers for different reasons. The figure
//! layout asks how wide a tick label is in order to size the margin it sits
//! in, and *both* backends have to agree on that or a stacked figure's panels
//! would not line up. The raster backend additionally needs the glyphs
//! themselves, which it takes from the same font atlas egui rasterizes for the
//! screen: laying out at the export's own `pixels_per_point` makes the atlas
//! entry and the destination pixels the same size, so text lands in the image
//! as crisply as it does in the app.
//!
//! The SVG backend cannot embed a font, so its text is drawn by the viewer in
//! whatever sans-serif it has. The metrics here are then an estimate rather
//! than the truth, which is why [`TextEngine::measure`] is only ever used for
//! *margins* -- room around text -- and never to position two things against
//! each other. See `svg::SvgCanvas` for the safety margin on top.

use std::sync::Arc;

use egui::epaint::text::{FontDefinitions, Fonts, TextOptions};
use egui::{Color32, FontId, Galley, Vec2};

/// Smallest font we will lay out; a zero or negative size is a division by
/// zero deep inside the shaper.
const MIN_FONT_SIZE: f32 = 1.0;

pub struct TextEngine {
    fonts: Fonts,
    pixels_per_point: f32,
}

impl TextEngine {
    /// `pixels_per_point` is the export's own scale: device pixels per figure
    /// unit. The raster backend passes its scale so glyphs are rasterized at
    /// output resolution; the SVG backend passes 1.0, since it only measures.
    pub fn new(pixels_per_point: f32) -> Self {
        let options = TextOptions {
            // Sub-pixel binning trades atlas space for even kerning on screen.
            // An exported figure is laid out once and never scrolled, and the
            // extra glyph copies only blur it.
            subpixel_binning: false,
            ..Default::default()
        };
        Self {
            fonts: Fonts::new(options, FontDefinitions::default()),
            pixels_per_point: pixels_per_point.max(0.05),
        }
    }

    pub fn pixels_per_point(&self) -> f32 {
        self.pixels_per_point
    }

    pub fn layout(&mut self, text: &str, size: f32, color: Color32) -> Arc<Galley> {
        let font = FontId::proportional(size.max(MIN_FONT_SIZE));
        self.fonts
            .with_pixels_per_point(self.pixels_per_point)
            .layout_no_wrap(text.to_owned(), font, color)
    }

    /// Size of `text` in figure units.
    pub fn measure(&mut self, text: &str, size: f32) -> Vec2 {
        self.layout(text, size, Color32::WHITE).rect.size()
    }

    /// Distance from the top of a line of text down to its baseline, which is
    /// what SVG positions text by.
    pub fn ascent(&mut self, size: f32) -> f32 {
        let galley = self.layout("Xg", size, Color32::WHITE);
        galley
            .rows
            .first()
            .and_then(|row| row.row.glyphs.first())
            .map_or(size * 0.8, |glyph| glyph.pos.y)
    }

    /// Line height in figure units.
    pub fn line_height(&mut self, size: f32) -> f32 {
        self.measure("Xg", size).y
    }

    /// The font atlas, as it stands after everything has been laid out.
    ///
    /// Cloning it is not cheap, so the raster backend takes it once, at the
    /// end, rather than per glyph -- which is also the only correct moment,
    /// since a glyph is only in the atlas once it has been laid out.
    pub fn atlas(&self) -> egui::ColorImage {
        self.fonts.image()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_measures_wider_the_more_of_it_there_is() {
        let mut engine = TextEngine::new(1.0);
        let one = engine.measure("40", 12.0);
        let many = engine.measure("40.000", 12.0);
        assert!(many.x > one.x, "{many:?} vs {one:?}");
        assert!((many.y - one.y).abs() < 0.01, "one line either way");
        assert!(one.y > 0.0 && one.x > 0.0);
    }

    #[test]
    fn the_baseline_sits_inside_the_line() {
        let mut engine = TextEngine::new(2.0);
        let height = engine.line_height(10.0);
        let ascent = engine.ascent(10.0);
        assert!(ascent > 0.0 && ascent < height, "ascent {ascent}, height {height}");
    }

    /// A size of zero reaches the shaper as a division by zero; the exporter
    /// clamps rather than checking at every call site.
    #[test]
    fn a_degenerate_font_size_does_not_panic() {
        let mut engine = TextEngine::new(1.0);
        assert!(engine.measure("x", 0.0).x > 0.0);
        assert!(engine.measure("x", -3.0).x > 0.0);
    }
}
