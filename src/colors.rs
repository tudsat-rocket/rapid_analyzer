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

/// Relative luminance every marker colour is brought to: 4:1 against a white
/// page, and the same against the dark theme's plot background.
const MARKER_LUMINANCE: f32 = 0.2;

/// The colour of the `n`th marker ever placed.
///
/// Markers are not drawn from [`PALETTE`]: there are eight entries in it, the
/// series in the graph already wear them, and a ninth marker would be the
/// first one again. Instead the hue advances by the golden angle, which never
/// lands on a hue it has used and puts each new one as far as it can from the
/// ones before it. Every hue is then brought to the same luminance -- mixed
/// towards white if it is too dark (blue), scaled down if it is too bright
/// (yellow) -- so a marker is as legible in one theme as in the other, and on
/// the page an exported figure is printed on.
pub fn marker_color(n: usize) -> Color32 {
    const GOLDEN: f64 = 0.618_033_988_749_895;
    let hue = ((0.58 + n as f64 * GOLDEN).fract() * 6.0) as f32;
    // The hue wheel, in linear light, a little short of fully saturated.
    let channel = |shift: f32| {
        let k = (shift + hue) % 6.0;
        1.0 - 0.9 * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    let mut rgb = [channel(5.0), channel(3.0), channel(1.0)];
    let luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    for c in &mut rgb {
        *c = if luminance > MARKER_LUMINANCE {
            *c * MARKER_LUMINANCE / luminance
        } else {
            *c + (1.0 - *c) * (MARKER_LUMINANCE - luminance) / (1.0 - luminance)
        };
    }
    egui::Rgba::from_rgb(rgb[0], rgb[1], rgb[2]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color32) -> f32 {
        let c = egui::Rgba::from(c);
        0.2126 * c.r() + 0.7152 * c.g() + 0.0722 * c.b()
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (hi, lo) = (luminance(a).max(luminance(b)), luminance(a).min(luminance(b)));
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn every_marker_colour_is_legible_in_both_themes() {
        for n in 0..200 {
            let c = marker_color(n);
            assert!(contrast(c, Color32::WHITE) >= 3.0, "marker {n} {c:?} on white");
            assert!(contrast(c, Color32::from_gray(0x1B)) >= 3.0, "marker {n} {c:?} on dark");
        }
    }

    #[test]
    fn no_two_markers_share_a_colour() {
        let mut seen = std::collections::HashSet::new();
        for n in 0..200 {
            assert!(seen.insert(marker_color(n)), "marker {n} repeats an earlier colour");
        }
    }

    #[test]
    fn consecutive_markers_are_far_apart() {
        // The ones placed one after another are the ones that end up next to
        // each other on screen.
        for n in 0..50 {
            let (a, b) = (marker_color(n).to_array(), marker_color(n + 1).to_array());
            let distance: i32 = (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum();
            assert!(distance > 100, "markers {n} and {} are {a:?} and {b:?}", n + 1);
        }
    }
}
