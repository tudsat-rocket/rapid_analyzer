//! The manual CAN signal picker.
//!
//! [`crate::can::iocan`] covers the vehicle's own protocol, but a log will
//! carry traffic from anything else wired to the same bus -- a bought-in
//! sensor, a motor controller, another team's board -- whose layout only its
//! datasheet knows. This window is where that knowledge gets entered: pick an
//! identifier, say which bytes of it are the number and how to scale it, and
//! get a series that behaves like any other.

use egui::Widget as _;

use crate::can::{CanFrames, CanIdentifier, FieldKind, SignalSpec};
use crate::model::SourceId;
use crate::series::TimeSeries;

pub enum BuilderAction {
    None,
    /// Add this series to the source and open it in a graph.
    Plot(TimeSeries),
    Close,
}

pub struct CanBuilder {
    source: SourceId,
    spec: SignalSpec,
    /// Snapshot of what is on the bus, taken once: recounting a hundred
    /// thousand frames every frame of UI would be the most expensive thing
    /// the app does.
    identifiers: Vec<CanIdentifier>,
    /// The spec the preview below was computed for, so it is recomputed when
    /// the user changes something and not on every repaint.
    previewed: Option<SignalSpec>,
    preview: Option<Preview>,
}

/// What the current spec would produce, shown before anything is added -- an
/// offset that lands on padding otherwise looks exactly like a working one
/// until the (empty) graph opens.
struct Preview {
    samples: usize,
    min: f64,
    max: f64,
}

impl CanBuilder {
    pub fn new(source: SourceId, frames: &CanFrames) -> Self {
        let identifiers = frames.identifiers();
        let spec = SignalSpec {
            id: identifiers.first().map_or(0, |i| i.id),
            ..Default::default()
        };
        Self {
            source,
            spec,
            identifiers,
            previewed: None,
            preview: None,
        }
    }

    pub fn source(&self) -> SourceId {
        self.source
    }

    pub fn show(&mut self, ctx: &egui::Context, source_name: &str, frames: &CanFrames) -> BuilderAction {
        let mut open = true;
        let mut action = BuilderAction::None;

        egui::Window::new(format!("CAN signal -- {source_name}"))
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .show(ctx, |ui| {
                action = self.contents(ui, frames);
            });

        if !open {
            return BuilderAction::Close;
        }
        action
    }

    fn contents(&mut self, ui: &mut egui::Ui, frames: &CanFrames) -> BuilderAction {
        if self.identifiers.is_empty() {
            ui.label("This log carries no CAN frames.");
            return BuilderAction::None;
        }

        let selected = self
            .identifiers
            .iter()
            .find(|i| i.id == self.spec.id)
            .map_or_else(|| format!("0x{:03X}", self.spec.id), CanIdentifier::label);

        egui::Grid::new("can_signal_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Identifier");
            egui::ComboBox::from_id_salt("can_id")
                .selected_text(selected)
                .width(340.0)
                .show_ui(ui, |ui| {
                    for identifier in &self.identifiers {
                        ui.selectable_value(&mut self.spec.id, identifier.id, identifier.label());
                    }
                });
            ui.end_row();

            // Only worth asking about when the log actually has two buses;
            // otherwise the answer is always "the one bus there is".
            let buses: Vec<u8> = self.identifiers.iter().fold(Vec::new(), |mut acc, i| {
                if !acc.contains(&i.bus) {
                    acc.push(i.bus);
                }
                acc
            });
            if buses.len() > 1 {
                ui.label("Bus");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.spec.bus, None, "any");
                    for bus in buses {
                        ui.selectable_value(&mut self.spec.bus, Some(bus), format!("{bus}"));
                    }
                });
                ui.end_row();
            }

            ui.label("Field");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("can_field_kind")
                    .selected_text(self.spec.kind.name())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for kind in FieldKind::ALL {
                            let selected = self.spec.kind.name() == kind.name();
                            if ui.selectable_label(selected, kind.name()).clicked() {
                                // Keep the chosen bit when switching back and
                                // forth between bit and byte fields.
                                self.spec.kind = match (kind, self.spec.kind) {
                                    (FieldKind::Bit(_), FieldKind::Bit(bit)) => FieldKind::Bit(bit),
                                    _ => *kind,
                                };
                            }
                        }
                    });
                ui.label("at byte");
                // Widening the field can leave the offset pointing past the
                // end of a frame; pull it back rather than silently yielding
                // a signal with no samples.
                let max_offset = 8usize.saturating_sub(self.spec.kind.width());
                self.spec.offset = self.spec.offset.min(max_offset);
                ui.add(egui::DragValue::new(&mut self.spec.offset).range(0..=max_offset).speed(0.1));
                if let FieldKind::Bit(bit) = self.spec.kind {
                    let mut bit = bit;
                    ui.label("bit");
                    ui.add(egui::DragValue::new(&mut bit).range(0..=7).speed(0.1));
                    self.spec.kind = FieldKind::Bit(bit);
                }
                if self.spec.kind.has_byte_order() {
                    ui.separator();
                    ui.selectable_value(&mut self.spec.big_endian, false, "LE")
                        .on_hover_text("Little endian (this vehicle's own protocol, and most others)");
                    ui.selectable_value(&mut self.spec.big_endian, true, "BE")
                        .on_hover_text("Big endian (network byte order, common in J1939/OBD)");
                }
            });
            ui.end_row();

            ui.label("Value")
                .on_hover_text("The plotted value is raw * scale + offset");
            ui.horizontal(|ui| {
                ui.label("raw ×");
                ui.add(egui::DragValue::new(&mut self.spec.scale).speed(0.001).max_decimals(9));
                ui.label("+");
                ui.add(egui::DragValue::new(&mut self.spec.bias).speed(0.01).max_decimals(9));
                ui.label("unit");
                ui.add(egui::TextEdit::singleline(&mut self.spec.unit).desired_width(60.0).hint_text("bar"));
            });
            ui.end_row();

            ui.label("Name");
            let hint = self.spec.default_name();
            egui::TextEdit::singleline(&mut self.spec.name)
                .hint_text(hint)
                .desired_width(340.0)
                .ui(ui);
            ui.end_row();
        });

        // Recompute only when something changed: one pass over every frame in
        // the log is cheap once and wasteful sixty times a second.
        if self.previewed.as_ref() != Some(&self.spec) {
            self.preview = preview(frames, &self.spec);
            self.previewed = Some(self.spec.clone());
        }

        ui.separator();
        match &self.preview {
            Some(p) => {
                ui.label(format!(
                    "{} samples, {:.4} .. {:.4}{}",
                    p.samples,
                    p.min,
                    p.max,
                    if self.spec.unit.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" {}", self.spec.unit.trim())
                    }
                ));
            }
            None => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "No samples: no frame with this identifier is long enough to hold that field.",
                );
            }
        }

        ui.horizontal(|ui| {
            let enabled = self.preview.is_some();
            let plot = ui
                .add_enabled(enabled, egui::Button::new("Plot it"))
                .on_hover_text("Add this signal to the source's series and open it in a graph");
            if plot.clicked() {
                return BuilderAction::Plot(frames.extract(&self.spec));
            }
            if ui.button("Close").clicked() {
                return BuilderAction::Close;
            }
            BuilderAction::None
        })
        .inner
    }
}

fn preview(frames: &CanFrames, spec: &SignalSpec) -> Option<Preview> {
    let series = frames.extract(spec);
    let (min, max) = series.value_bounds_in_range(f64::NEG_INFINITY, f64::INFINITY, 0.0)?;
    Some(Preview {
        samples: series.len(),
        min,
        max,
    })
}
