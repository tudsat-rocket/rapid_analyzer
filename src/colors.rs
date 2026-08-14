use egui::Color32;

/// A small categorical palette, cycled by index, used both for per-source
/// accent colors and per-series plot line colors.
const PALETTE: &[Color32] = &[
    Color32::from_rgb(0x4C, 0x9A, 0xFF),
    Color32::from_rgb(0xFF, 0x8A, 0x3D),
    Color32::from_rgb(0x4C, 0xD9, 0x7B),
    Color32::from_rgb(0xFF, 0x5C, 0x8A),
    Color32::from_rgb(0xB0, 0x7B, 0xFF),
    Color32::from_rgb(0xFF, 0xD5, 0x3D),
    Color32::from_rgb(0x3D, 0xD9, 0xD9),
    Color32::from_rgb(0xE0, 0x7B, 0xE0),
];

pub fn color_for_index(i: usize) -> Color32 {
    PALETTE[i % PALETTE.len()]
}
