use egui::Color32;

/// A small categorical palette, cycled by index, used both for per-source
/// accent colors and per-series plot line colors.
///
/// A source's colour is picked once, when it is imported, and a plot line's
/// when it is added -- neither is re-picked when the theme changes, and an
/// exported figure is usually the opposite theme of the app it was exported
/// from. So there is one palette rather than a light and a dark one, and every
/// entry is a mid-tone that clears 3:1 contrast against *both* a white page
/// and the dark theme's near-black plot background.
const PALETTE: &[Color32] = &[
    Color32::from_rgb(0x2E, 0x7E, 0xE6), // blue
    Color32::from_rgb(0xE8, 0x59, 0x0C), // orange
    Color32::from_rgb(0x22, 0xA0, 0x5E), // green
    Color32::from_rgb(0xE6, 0x49, 0x80), // pink
    Color32::from_rgb(0x8B, 0x5C, 0xF6), // violet
    Color32::from_rgb(0xB0, 0x88, 0x00), // gold
    Color32::from_rgb(0x0E, 0x94, 0x94), // teal
    Color32::from_rgb(0xC2, 0x5B, 0xC2), // magenta
];

pub fn color_for_index(i: usize) -> Color32 {
    PALETTE[i % PALETTE.len()]
}
