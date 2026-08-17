//! Raw CAN frames carried in a log, and the two ways of getting numbers out
//! of them.
//!
//! A `.tlog` carries bus traffic as `CAN_FRAME` messages: an identifier and up
//! to 8 opaque bytes. Expanding those the way every other MAVLink message is
//! expanded gives `CAN_FRAME.data[0]` -- one line interleaving byte 0 of every
//! frame from every node, which says nothing. Frames have to be split by
//! identifier first and then read as whatever that identifier means, which is
//! knowledge no dialect XML carries. So there are two paths:
//!
//! * [`iocan`] knows this vehicle's own protocol and turns the frames into
//!   named signals -- a node's high current outputs, its sensor slots, its
//!   rails -- with nothing for the user to specify.
//! * [`SignalSpec`] is the fallback for every other bus on the vehicle: name
//!   an identifier, a byte offset and a type, and get a series out.

pub mod iocan;

use crate::series::TimeSeries;

/// One frame as it came off the bus.
#[derive(Clone, Copy, Debug)]
pub struct CanFrame {
    /// Absolute UTC seconds, from the log record that carried the frame.
    pub t_utc: f64,
    pub id: u32,
    pub bus: u8,
    /// Bytes actually transmitted; `data` past this is padding.
    pub len: u8,
    pub data: [u8; 8],
}

/// Every CAN frame a log carried, in arrival order.
///
/// Kept whole rather than decoded away at import time: which bytes of which
/// identifier are worth plotting is a question the user answers afterwards,
/// and re-reading a 9 MB log to answer it again would be absurd. ~24 bytes a
/// frame puts a busy 15-minute log at a couple of megabytes.
#[derive(Default)]
pub struct CanFrames {
    frames: Vec<CanFrame>,
}

impl CanFrames {
    pub fn push(&mut self, frame: CanFrame) {
        self.frames.push(frame);
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn frames(&self) -> &[CanFrame] {
        &self.frames
    }

    /// Every identifier seen, with how often and what it is, for the signal
    /// picker to offer. Sorted by bus then identifier, which for this protocol
    /// groups a node's traffic together.
    pub fn identifiers(&self) -> Vec<CanIdentifier> {
        let mut out: Vec<CanIdentifier> = Vec::new();
        for frame in &self.frames {
            match out.iter_mut().find(|i| i.bus == frame.bus && i.id == frame.id) {
                Some(existing) => {
                    existing.count += 1;
                    existing.max_len = existing.max_len.max(frame.len);
                }
                None => out.push(CanIdentifier {
                    bus: frame.bus,
                    id: frame.id,
                    count: 1,
                    max_len: frame.len,
                    description: iocan::describe(frame.id),
                }),
            }
        }
        out.sort_by_key(|i| (i.bus, i.id));
        out
    }

    /// Pulls one field out of every frame matching `spec`.
    pub fn extract(&self, spec: &SignalSpec) -> TimeSeries {
        let mut points = Vec::new();
        for frame in &self.frames {
            if frame.id != spec.id || spec.bus.is_some_and(|bus| bus != frame.bus) {
                continue;
            }
            if let Some(raw) = spec.kind.read(frame, spec.offset, spec.big_endian) {
                points.push([frame.t_utc, raw * spec.scale + spec.bias]);
            }
        }
        let unit = (!spec.unit.trim().is_empty()).then(|| spec.unit.trim().to_string());
        TimeSeries::from_points(spec.series_name(), points).with_unit(unit)
    }
}

/// One identifier seen on the bus.
pub struct CanIdentifier {
    pub bus: u8,
    pub id: u32,
    pub count: usize,
    pub max_len: u8,
    /// What the protocol says this identifier is, when it recognizes it.
    pub description: Option<String>,
}

impl CanIdentifier {
    pub fn label(&self) -> String {
        let base = format!("0x{:03X}  (bus {}, {} frames)", self.id, self.bus, self.count);
        match &self.description {
            Some(what) => format!("{base}  --  {what}"),
            None => base,
        }
    }
}

/// How to read a number out of a frame's payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    /// A single bit of the byte at the field's offset.
    Bit(u8),
}

impl FieldKind {
    /// Every kind the picker offers. `Bit` stands in for all eight bits; which
    /// one is chosen separately.
    pub const ALL: &'static [Self] = &[
        Self::U8,
        Self::I8,
        Self::U16,
        Self::I16,
        Self::U32,
        Self::I32,
        Self::F32,
        Self::Bit(0),
    ];

    pub fn width(self) -> usize {
        match self {
            Self::U8 | Self::I8 | Self::Bit(_) => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::F32 => "f32",
            Self::Bit(_) => "bit",
        }
    }

    /// Whether the byte order matters -- it doesn't for a single byte, and
    /// offering the choice there only invites the question.
    pub fn has_byte_order(self) -> bool {
        self.width() > 1
    }

    /// Reads the field, or `None` if the frame is too short to contain it.
    ///
    /// Short frames are skipped rather than zero-filled: a node that sends a
    /// 1-byte heartbeat on an identifier does not have a `u16` at offset 0,
    /// and inventing one would draw a line of zeroes that looks like data.
    fn read(self, frame: &CanFrame, offset: usize, big_endian: bool) -> Option<f64> {
        let end = offset.checked_add(self.width())?;
        if end > frame.len as usize || end > frame.data.len() {
            return None;
        }
        let bytes = &frame.data[offset..end];
        let word = |n: usize| -> u64 {
            let mut value = 0u64;
            for i in 0..n {
                let byte = bytes[if big_endian { i } else { n - 1 - i }] as u64;
                value = (value << 8) | byte;
            }
            value
        };
        Some(match self {
            Self::U8 => bytes[0] as f64,
            Self::I8 => bytes[0] as i8 as f64,
            Self::U16 => word(2) as f64,
            Self::I16 => word(2) as u16 as i16 as f64,
            Self::U32 => word(4) as f64,
            Self::I32 => word(4) as u32 as i32 as f64,
            Self::F32 => f32::from_bits(word(4) as u32) as f64,
            Self::Bit(bit) => ((bytes[0] >> (bit & 7)) & 1) as f64,
        })
    }
}

/// A user-specified field of a user-specified identifier: the escape hatch for
/// any bus traffic the built-in protocol doesn't cover.
#[derive(Clone, PartialEq, Debug)]
pub struct SignalSpec {
    /// `None` matches the identifier on every bus in the log.
    pub bus: Option<u8>,
    pub id: u32,
    pub offset: usize,
    pub kind: FieldKind,
    pub big_endian: bool,
    /// Raw counts are rarely the quantity of interest; `value * scale + bias`
    /// is what a datasheet's conversion looks like.
    pub scale: f64,
    pub bias: f64,
    /// Empty means "call it whatever [`Self::default_name`] says".
    pub name: String,
    pub unit: String,
}

impl Default for SignalSpec {
    fn default() -> Self {
        Self {
            bus: None,
            id: 0,
            offset: 0,
            kind: FieldKind::U16,
            big_endian: false,
            scale: 1.0,
            bias: 0.0,
            name: String::new(),
            unit: String::new(),
        }
    }
}

impl SignalSpec {
    /// A name that says exactly where the numbers came from, so an unnamed
    /// signal is still identifiable a week later.
    pub fn default_name(&self) -> String {
        let bus = match self.bus {
            Some(bus) => format!("bus{bus}:"),
            None => String::new(),
        };
        let kind = match self.kind {
            FieldKind::Bit(bit) => format!("bit{bit}"),
            other if other.has_byte_order() && self.big_endian => format!("{}be", other.name()),
            other => other.name().to_string(),
        };
        format!("CAN[{bus}0x{:03X}].{kind}@{}", self.id, self.offset)
    }

    pub fn series_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            self.default_name()
        } else {
            name.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u32, data: [u8; 8]) -> CanFrame {
        CanFrame {
            t_utc: 0.0,
            id,
            bus: 1,
            len: 8,
            data,
        }
    }

    #[test]
    fn fields_are_read_little_endian_by_default() {
        let f = frame(0x243, [0x00, 0x80, 0x98, 0x08, 0x34, 0x12, 0xFF, 0xFF]);
        assert_eq!(FieldKind::U16.read(&f, 0, false), Some(32768.0));
        assert_eq!(FieldKind::U16.read(&f, 2, false), Some(2200.0));
        assert_eq!(FieldKind::U16.read(&f, 4, true), Some(0x3412 as f64));
        assert_eq!(FieldKind::I16.read(&f, 6, false), Some(-1.0));
        assert_eq!(FieldKind::I8.read(&f, 6, false), Some(-1.0));
        assert_eq!(FieldKind::U8.read(&f, 1, false), Some(128.0));
        assert_eq!(FieldKind::Bit(7).read(&f, 1, false), Some(1.0));
        assert_eq!(FieldKind::Bit(0).read(&f, 1, false), Some(0.0));
    }

    #[test]
    fn floats_round_trip_through_the_payload() {
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&(-12.5f32).to_le_bytes());
        data[4..].copy_from_slice(&(0.25f32).to_be_bytes());
        let f = frame(0x100, data);
        assert_eq!(FieldKind::F32.read(&f, 0, false), Some(-12.5));
        assert_eq!(FieldKind::F32.read(&f, 4, true), Some(0.25));
    }

    #[test]
    fn a_field_past_the_end_of_a_short_frame_is_no_sample() {
        let mut f = frame(0x703, [5, 0, 0, 0, 0, 0, 0, 0]);
        f.len = 1;
        assert_eq!(FieldKind::U8.read(&f, 0, false), Some(5.0));
        assert_eq!(FieldKind::U16.read(&f, 0, false), None);
        assert_eq!(FieldKind::U8.read(&f, 1, false), None);
        // ... and nothing reads past the payload however long the frame claims.
        f.len = 8;
        assert_eq!(FieldKind::U32.read(&f, 6, false), None);
    }

    #[test]
    fn extraction_keeps_only_the_matching_identifier() {
        let mut frames = CanFrames::default();
        for (i, id) in [0x243u32, 0x244, 0x243].into_iter().enumerate() {
            frames.push(CanFrame {
                t_utc: i as f64,
                id,
                bus: 1,
                len: 8,
                data: [i as u8, 0, 0, 0, 0, 0, 0, 0],
            });
        }
        let series = frames.extract(&SignalSpec {
            id: 0x243,
            kind: FieldKind::U8,
            ..Default::default()
        });
        assert_eq!(series.len(), 2);
        assert_eq!(series.value_at(0.0, 0.0), Some(0.0));
        assert_eq!(series.value_at(2.0, 0.0), Some(2.0));
    }

    #[test]
    fn scale_and_bias_convert_raw_counts() {
        let mut frames = CanFrames::default();
        frames.push(frame(0x2E5, [0xD2, 0x27, 0, 0, 0, 0, 0, 0]));
        // 10194 mV read as volts.
        let series = frames.extract(&SignalSpec {
            id: 0x2E5,
            kind: FieldKind::U16,
            scale: 0.001,
            unit: "V".to_string(),
            ..Default::default()
        });
        assert_eq!(series.value_at(0.0, 0.0), Some(10.194));
        assert_eq!(series.unit.as_deref(), Some("V"));
    }

    #[test]
    fn an_unnamed_signal_names_itself_after_where_it_came_from() {
        let spec = SignalSpec {
            id: 0x243,
            offset: 2,
            kind: FieldKind::U16,
            ..Default::default()
        };
        assert_eq!(spec.series_name(), "CAN[0x243].u16@2");
        assert_eq!(
            SignalSpec {
                big_endian: true,
                bus: Some(1),
                ..spec.clone()
            }
            .series_name(),
            "CAN[bus1:0x243].u16be@2"
        );
        assert_eq!(
            SignalSpec {
                kind: FieldKind::Bit(3),
                offset: 0,
                ..spec.clone()
            }
            .series_name(),
            "CAN[0x243].bit3@0"
        );
        assert_eq!(
            SignalSpec {
                name: "  chamber pressure  ".to_string(),
                ..spec
            }
            .series_name(),
            "chamber pressure"
        );
    }

    #[test]
    fn identifiers_are_counted_and_described() {
        let mut frames = CanFrames::default();
        for id in [0x243u32, 0x243, 0x707] {
            frames.push(frame(id, [0; 8]));
        }
        let ids = frames.identifiers();
        assert_eq!(ids.len(), 2);
        assert_eq!((ids[0].id, ids[0].count), (0x243, 2));
        assert!(ids[0].description.as_deref().is_some_and(|d| d.contains("HcoState")), "{:?}", ids[0].description);
        assert_eq!((ids[1].id, ids[1].count), (0x707, 1));
    }
}
