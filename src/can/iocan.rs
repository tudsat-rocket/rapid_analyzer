//! The vehicle's own CAN protocol, as spoken by the IO boards.
//!
//! This mirrors `iocan-proto` (`ids.rs` and `tpdo.rs`) from the io board
//! firmware repository: the identifier layout
//!
//! ```text
//!   0x200 | (kind << 4) | node_id     process data out of a node
//!   0x580 + node_id                   SDO response
//!   0x600 + node_id                   SDO request
//!   0x700 + node_id                   heartbeat
//! ```
//!
//! and the fixed 8-byte, little-endian payload of each process-data kind.
//! It is a copy rather than a dependency because the firmware crate is a
//! `no_std` embedded workspace built for a different target; what it costs is
//! that a protocol change has to be made in both places, which is why the kind
//! table below is written out in full and cross-checked by
//! [`tests::the_kind_table_matches_the_identifier_layout`] rather than derived.
//!
//! Deliberately *not* decoded: SDO traffic (a request/response protocol, not
//! samples of anything) and any identifier outside the layout above -- those
//! are what [`super::SignalSpec`] is for.

use std::collections::HashMap;

use super::CanFrame;
use crate::series::TimeSeries;

const PDO_BASE: u32 = 0x200;
const SDO_RESPONSE_BASE: u32 = 0x580;
const SDO_REQUEST_BASE: u32 = 0x600;
const HEARTBEAT_BASE: u32 = 0x700;
const NODE_ID_MASK: u32 = 0x00F;

/// Number of sensor slots the protocol can carry, across `Sensor0`/`1`/`3`.
const PROTOCOL_SENSOR_SLOTS: usize = 12;

/// "No reading" for a calibrated sensor slot, and for a raw amplifier channel.
const SENSOR_INVALID: i16 = i16::MIN;
const RAW_INVALID: u16 = u16::MAX;

/// A high current output's pulse width word: these two values are sentinels
/// rather than widths, since every real width is far below `0x8000`.
const HCO_DIGITAL_ON: u16 = 0x8000;
const HCO_DIGITAL_OFF: u16 = 0x0000;

/// Bit 15 of a valve position word: the drive is released. The rest is the
/// position in promille.
const VALVE_UNPOWERED: u16 = 0x8000;
const VALVE_POSITION_MASK: u16 = 0x7FFF;

/// The fixed process-data table. The index is the `kind` field of the
/// identifier, so the order is the protocol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TpdoKind {
    ValveCommanded,
    ValveTarget,
    ValveMeasured,
    ValveStatus,
    HcoState,
    RawBus0A,
    RawBus0B,
    RawBus1A,
    RawBus1B,
    Sensor0,
    Sensor1,
    Sensor3,
    SensorUnits,
    I2cScan,
    RailVoltage,
    RailCurrent,
    Status,
    ValveCurrent,
}

const KINDS: [TpdoKind; 18] = [
    TpdoKind::ValveCommanded,
    TpdoKind::ValveTarget,
    TpdoKind::ValveMeasured,
    TpdoKind::ValveStatus,
    TpdoKind::HcoState,
    TpdoKind::RawBus0A,
    TpdoKind::RawBus0B,
    TpdoKind::RawBus1A,
    TpdoKind::RawBus1B,
    TpdoKind::Sensor0,
    TpdoKind::Sensor1,
    TpdoKind::Sensor3,
    TpdoKind::SensorUnits,
    TpdoKind::I2cScan,
    TpdoKind::RailVoltage,
    TpdoKind::RailCurrent,
    TpdoKind::Status,
    TpdoKind::ValveCurrent,
];

impl TpdoKind {
    fn from_index(index: u32) -> Option<Self> {
        KINDS.get(index as usize).copied()
    }

    fn name(self) -> &'static str {
        match self {
            Self::ValveCommanded => "ValveCommanded",
            Self::ValveTarget => "ValveTarget",
            Self::ValveMeasured => "ValveMeasured",
            Self::ValveStatus => "ValveStatus",
            Self::HcoState => "HcoState",
            Self::RawBus0A => "RawBus0A",
            Self::RawBus0B => "RawBus0B",
            Self::RawBus1A => "RawBus1A",
            Self::RawBus1B => "RawBus1B",
            Self::Sensor0 => "Sensor0",
            Self::Sensor1 => "Sensor1",
            Self::Sensor3 => "Sensor3",
            Self::SensorUnits => "SensorUnits",
            Self::I2cScan => "I2cScan",
            Self::RailVoltage => "RailVoltage",
            Self::RailCurrent => "RailCurrent",
            Self::Status => "Status",
            Self::ValveCurrent => "ValveCurrent",
        }
    }
}

/// What an identifier is, for the signal picker's list. `None` for anything
/// outside this protocol.
pub fn describe(id: u32) -> Option<String> {
    let node = id & NODE_ID_MASK;
    match id {
        PDO_BASE..SDO_RESPONSE_BASE => {
            let kind = TpdoKind::from_index((id - PDO_BASE) >> 4)?;
            Some(format!("node {node} {}", kind.name()))
        }
        SDO_RESPONSE_BASE..SDO_REQUEST_BASE => Some(format!("node {node} SDO response")),
        SDO_REQUEST_BASE..HEARTBEAT_BASE => Some(format!("node {node} SDO request")),
        HEARTBEAT_BASE..0x710 => Some(format!("node {node} heartbeat")),
        _ => None,
    }
}

/// Turns every recognized frame into named series.
///
/// The bus number joins the series name only when the log carries more than
/// one, since two IO buses use the same node ids for different boards but one
/// bus is the common case and `CAN_HCO[5]` reads better than `CAN_HCO[bus1:5]`.
pub fn decode(frames: &[CanFrame]) -> Vec<TimeSeries> {
    let mut collector = Collector::new(frames);
    for frame in frames {
        collector.push_frame(frame);
    }
    collector.finish()
}

/// Field names are written out rather than formatted so that a series key
/// stays `Copy` and the decode loop -- millions of samples on a long log --
/// never allocates.
macro_rules! names {
    ($prefix:literal, $suffix:literal, $($i:literal),+) => {
        [$(concat!($prefix, $i, $suffix)),+]
    };
}

const COMMANDED: [&str; 4] = names!("commanded", "", 0, 1, 2, 3);
const COMMANDED_UNPOWERED: [&str; 4] = names!("commanded", "_unpowered", 0, 1, 2, 3);
const TARGET: [&str; 4] = names!("target", "", 0, 1, 2, 3);
const TARGET_UNPOWERED: [&str; 4] = names!("target", "_unpowered", 0, 1, 2, 3);
const MEASURED: [&str; 4] = names!("measured", "", 0, 1, 2, 3);
const MEASURED_UNPOWERED: [&str; 4] = names!("measured", "_unpowered", 0, 1, 2, 3);
const VALVE_CURRENT: [&str; 4] = names!("current", "", 0, 1, 2, 3);
const VALVE_STATUS: [&str; 4] = names!("status", "", 0, 1, 2, 3);
const HCO_OWNER: [&str; 4] = names!("hco_owner", "", 0, 1, 2, 3);
/// The board silkscreen counts outputs from one; the firmware counts from zero.
/// A user reading a plot is looking at the silkscreen.
const HCO_OUT: [&str; 4] = names!("out", "", 1, 2, 3, 4);
const HCO_PWM: [&str; 4] = names!("out", "_pwm_us", 1, 2, 3, 4);
const SENSOR_SLOT: [&str; PROTOCOL_SENSOR_SLOTS] = names!("slot", "", 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11);
const ADC_BUS0: [&str; 8] = names!("bus0_ch", "", 0, 1, 2, 3, 4, 5, 6, 7);
const ADC_BUS1: [&str; 8] = names!("bus1_ch", "", 0, 1, 2, 3, 4, 5, 6, 7);
const RAIL_VOLTAGE: [&str; 3] = ["voltage_logic", "voltage_hco12", "voltage_hco34"];
const RAIL_CURRENT: [&str; 3] = ["current_logic", "current_hco12", "current_hco34"];

const VALVE: &str = "CAN_VALVE";
const HCO: &str = "CAN_HCO";
const SENSOR: &str = "CAN_SENSOR";
const ADC: &str = "CAN_ADC";
const RAIL: &str = "CAN_RAIL";
const STATUS: &str = "CAN_STATUS";
const I2C: &str = "CAN_I2C";
const NODE: &str = "CAN_NODE";

/// Promille of full travel, the unit all three valve position layers use.
const PROMILLE: &str = "‰";

/// What one series is: a signal of a node, on a bus.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    bus: u8,
    node: u8,
    group: &'static str,
    field: &'static str,
}

#[derive(Default)]
struct Samples {
    points: Vec<[f64; 2]>,
    unit: &'static str,
}

struct Collector {
    series: HashMap<Key, Samples>,
    /// Unit code per sensor slot, from each node's first `SensorUnits` frame.
    ///
    /// Resolved up front rather than as the frames arrive, so that the samples
    /// before a node's first `SensorUnits` frame are scaled the same as the
    /// ones after it -- otherwise a series would step by a factor of 100 in
    /// the middle for no physical reason. A node that re-declares different
    /// units mid-log would be misread, which is not a thing that happens
    /// outside of a calibration session.
    sensor_units: HashMap<(u8, u8), [u8; PROTOCOL_SENSOR_SLOTS]>,
    multi_bus: bool,
}

impl Collector {
    fn new(frames: &[CanFrame]) -> Self {
        let mut sensor_units = HashMap::new();
        let mut buses: Vec<u8> = Vec::new();
        for frame in frames {
            if !buses.contains(&frame.bus) {
                buses.push(frame.bus);
            }
            if frame.len as usize == 8
                && let Some((node, kind)) = process_data(frame.id)
                && kind == TpdoKind::SensorUnits
            {
                sensor_units
                    .entry((frame.bus, node))
                    .or_insert_with(|| unpack_2bit(&frame.data[..3]));
            }
        }
        Self {
            series: HashMap::new(),
            sensor_units,
            multi_bus: buses.len() > 1,
        }
    }

    fn push(&mut self, frame: &CanFrame, group: &'static str, field: &'static str, unit: &'static str, value: f64) {
        let key = Key {
            bus: frame.bus,
            node: frame.id as u8 & NODE_ID_MASK as u8,
            group,
            field,
        };
        let samples = self.series.entry(key).or_default();
        samples.unit = unit;
        samples.points.push([frame.t_utc, value]);
    }

    /// Both halves of a valve position word: where the valve is, and whether
    /// it is being held there or has been released.
    fn push_position(&mut self, frame: &CanFrame, position: [&'static str; 4], unpowered: [&'static str; 4]) {
        for (i, word) in u16x4(&frame.data).into_iter().enumerate() {
            self.push(frame, VALVE, position[i], PROMILLE, (word & VALVE_POSITION_MASK) as f64);
            self.push(
                frame,
                VALVE,
                unpowered[i],
                "",
                ((word & VALVE_UNPOWERED) != 0) as u8 as f64,
            );
        }
    }

    /// One window of the raw amplifier readings. `RAW_INVALID` means the
    /// channel was never read, which is a gap in the series, not a value.
    fn push_raw_adc(&mut self, frame: &CanFrame, names: [&'static str; 8], window: usize) {
        for (i, raw) in u16x4(&frame.data).into_iter().enumerate() {
            if raw != RAW_INVALID {
                self.push(frame, ADC, names[window * 4 + i], "counts", raw as f64);
            }
        }
    }

    /// One window of the calibrated sensor slots, in whatever unit the node
    /// said it reports them in.
    fn push_sensors(&mut self, frame: &CanFrame, window: usize) {
        let node = frame.id as u8 & NODE_ID_MASK as u8;
        let codes = self.sensor_units.get(&(frame.bus, node)).copied();
        for (i, raw) in i16x4(&frame.data).into_iter().enumerate() {
            let slot = window * 4 + i;
            if raw == SENSOR_INVALID || slot >= PROTOCOL_SENSOR_SLOTS {
                continue;
            }
            let (unit, scale) = sensor_unit(codes.map(|c| c[slot]));
            self.push(frame, SENSOR, SENSOR_SLOT[slot], unit, raw as f64 * scale);
        }
    }

    fn push_frame(&mut self, frame: &CanFrame) {
        if frame.id > 0x7FF {
            // An extended (29-bit) identifier is not this protocol.
            return;
        }
        if (HEARTBEAT_BASE..HEARTBEAT_BASE + 16).contains(&frame.id) {
            // Always 0x05 ("operational"); the information is that it arrived
            // at all, so a series of it shows exactly when a node dropped off.
            if frame.len >= 1 {
                self.push(frame, NODE, "nmt_state", "", frame.data[0] as f64);
            }
            return;
        }
        let Some((_, kind)) = process_data(frame.id) else {
            return;
        };
        // Every process-data frame is a full 8 bytes by definition; a short
        // one is a different protocol reusing the identifier.
        if frame.len as usize != 8 {
            return;
        }

        match kind {
            TpdoKind::ValveCommanded => self.push_position(frame, COMMANDED, COMMANDED_UNPOWERED),
            TpdoKind::ValveTarget => self.push_position(frame, TARGET, TARGET_UNPOWERED),
            TpdoKind::ValveMeasured => self.push_position(frame, MEASURED, MEASURED_UNPOWERED),
            TpdoKind::ValveCurrent => {
                for (i, ma) in u16x4(&frame.data).into_iter().enumerate() {
                    self.push(frame, VALVE, VALVE_CURRENT[i], "mA", ma as f64);
                }
            }
            TpdoKind::ValveStatus => {
                for (i, status) in unpack_nibbles(&frame.data[..2]).into_iter().enumerate() {
                    self.push(frame, VALVE, VALVE_STATUS[i], "", status as f64);
                }
                for (i, owner) in unpack_nibbles(&frame.data[2..4]).into_iter().enumerate() {
                    self.push(frame, VALVE, HCO_OWNER[i], "", owner as f64);
                }
                self.push(frame, VALVE, "relief_state", "", frame.data[4] as f64);
            }
            TpdoKind::HcoState => {
                for (i, word) in u16x4(&frame.data).into_iter().enumerate() {
                    // Driven or not is the question asked of every output;
                    // the pulse width only exists for the ones on PWM, so a
                    // digital output gets no width series at all rather than
                    // one that is secretly a sentinel.
                    let driving = word != HCO_DIGITAL_OFF;
                    self.push(frame, HCO, HCO_OUT[i], "", driving as u8 as f64);
                    if word != HCO_DIGITAL_ON && word != HCO_DIGITAL_OFF {
                        self.push(frame, HCO, HCO_PWM[i], "us", word as f64);
                    }
                }
            }
            TpdoKind::RawBus0A => self.push_raw_adc(frame, ADC_BUS0, 0),
            TpdoKind::RawBus0B => self.push_raw_adc(frame, ADC_BUS0, 1),
            TpdoKind::RawBus1A => self.push_raw_adc(frame, ADC_BUS1, 0),
            TpdoKind::RawBus1B => self.push_raw_adc(frame, ADC_BUS1, 1),
            TpdoKind::Sensor0 => self.push_sensors(frame, 0),
            TpdoKind::Sensor1 => self.push_sensors(frame, 1),
            TpdoKind::Sensor3 => self.push_sensors(frame, 2),
            // Read in `Collector::new`; on its own it is a constant.
            TpdoKind::SensorUnits => {}
            TpdoKind::I2cScan => {
                let words = u16x4(&frame.data);
                self.push(frame, I2C, "present_bus0", "", words[0] as f64);
                self.push(frame, I2C, "present_bus1", "", words[1] as f64);
                self.push(frame, I2C, "sweeps", "", words[2] as f64);
            }
            TpdoKind::RailVoltage => {
                for (i, mv) in u16x4(&frame.data).into_iter().take(3).enumerate() {
                    self.push(frame, RAIL, RAIL_VOLTAGE[i], "mV", mv as f64);
                }
            }
            TpdoKind::RailCurrent => {
                for (i, ma) in u16x4(&frame.data).into_iter().take(3).enumerate() {
                    self.push(frame, RAIL, RAIL_CURRENT[i], "mA", ma as f64);
                }
            }
            TpdoKind::Status => {
                self.push(frame, STATUS, "link_state", "", frame.data[0] as f64);
                self.push(frame, STATUS, "raw_debug", "", (frame.data[1] != 0) as u8 as f64);
                self.push(frame, STATUS, "stalled_mask", "", frame.data[2] as f64);
                let ms = u32::from_le_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
                self.push(frame, STATUS, "ms_since_heartbeat", "ms", ms as f64);
            }
        }
    }

    fn finish(self) -> Vec<TimeSeries> {
        let multi_bus = self.multi_bus;
        let mut out: Vec<TimeSeries> = self
            .series
            .into_iter()
            .filter(|(_, samples)| !samples.points.is_empty())
            .map(|(key, samples)| {
                let instance = if multi_bus {
                    format!("bus{}:{}", key.bus, key.node)
                } else {
                    key.node.to_string()
                };
                let name = format!("{}[{instance}].{}", key.group, key.field);
                let unit = (!samples.unit.is_empty()).then(|| samples.unit.to_string());
                TimeSeries::from_points(name, samples.points).with_unit(unit)
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Splits a process-data identifier into its node and kind.
fn process_data(id: u32) -> Option<(u8, TpdoKind)> {
    if !(PDO_BASE..SDO_RESPONSE_BASE).contains(&id) {
        return None;
    }
    let kind = TpdoKind::from_index((id - PDO_BASE) >> 4)?;
    Some(((id & NODE_ID_MASK) as u8, kind))
}

/// Display unit and the factor raw counts are multiplied by to reach it, for
/// a sensor slot's declared unit code. An unknown or unseen code is left as
/// raw counts rather than guessed at.
fn sensor_unit(code: Option<u8>) -> (&'static str, f64) {
    match code {
        Some(0) => ("bar", 0.01),
        Some(1) => ("bar", 0.1),
        Some(2) => ("°C", 0.01),
        _ => ("counts", 1.0),
    }
}

fn u16x4(data: &[u8; 8]) -> [u16; 4] {
    let mut out = [0u16; 4];
    for (slot, chunk) in out.iter_mut().zip(data.chunks_exact(2)) {
        *slot = u16::from_le_bytes([chunk[0], chunk[1]]);
    }
    out
}

fn i16x4(data: &[u8; 8]) -> [i16; 4] {
    u16x4(data).map(|v| v as i16)
}

/// Four 4-bit values packed low nibble first into two bytes.
fn unpack_nibbles(bytes: &[u8]) -> [u8; 4] {
    [bytes[0] & 0xF, bytes[0] >> 4, bytes[1] & 0xF, bytes[1] >> 4]
}

/// Twelve 2-bit values packed low bits first into three bytes.
fn unpack_2bit(bytes: &[u8]) -> [u8; PROTOCOL_SENSOR_SLOTS] {
    let mut out = [0u8; PROTOCOL_SENSOR_SLOTS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = (bytes[i / 4] >> ((i % 4) * 2)) & 0b11;
    }
    out
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

    fn names(frames: &[CanFrame]) -> Vec<String> {
        decode(frames).into_iter().map(|s| s.name).collect()
    }

    fn value(frames: &[CanFrame], name: &str) -> Option<f64> {
        decode(frames)
            .into_iter()
            .find(|s| s.name == name)
            .and_then(|s| s.value_at(0.0, 0.0))
    }

    fn unit(frames: &[CanFrame], name: &str) -> Option<String> {
        decode(frames).into_iter().find(|s| s.name == name).and_then(|s| s.unit)
    }

    /// The identifier is `0x200 | kind << 4 | node`, and the table's order is
    /// the wire encoding -- a kind inserted in the wrong place would silently
    /// relabel every series after it.
    #[test]
    fn the_kind_table_matches_the_identifier_layout() {
        assert_eq!(process_data(0x200), Some((0, TpdoKind::ValveCommanded)));
        assert_eq!(process_data(0x243), Some((3, TpdoKind::HcoState)));
        assert_eq!(process_data(0x295), Some((5, TpdoKind::Sensor0)));
        assert_eq!(process_data(0x2BA), Some((10, TpdoKind::Sensor3)));
        assert_eq!(process_data(0x2E7), Some((7, TpdoKind::RailVoltage)));
        assert_eq!(process_data(0x30D), Some((13, TpdoKind::Status)));
        assert_eq!(process_data(0x31F), Some((15, TpdoKind::ValveCurrent)));
        // Past the last kind, and into the SDO range.
        assert_eq!(process_data(0x320), None);
        assert_eq!(process_data(0x583), None);
        assert_eq!(process_data(0x1FF), None);
    }

    #[test]
    fn identifiers_outside_the_protocol_are_left_to_the_manual_picker() {
        assert!(describe(0x243).is_some());
        assert!(describe(0x583).unwrap().contains("SDO response"));
        assert!(describe(0x703).unwrap().contains("heartbeat"));
        assert_eq!(describe(0x120), None);
        assert_eq!(describe(0x18FF_50E5), None);
        assert!(decode(&[frame(0x120, [1; 8])]).is_empty());
    }

    /// A real `HcoState` frame off node 5: output 1 energized, output 2 on a
    /// 2200 us servo pulse, output 3 energized, output 4 at 2000 us.
    #[test]
    fn high_current_outputs_decode_to_state_and_pulse_width() {
        let frames = [frame(0x245, [0x00, 0x80, 0x98, 0x08, 0x00, 0x80, 0xD0, 0x07])];
        assert_eq!(value(&frames, "CAN_HCO[5].out1"), Some(1.0));
        assert_eq!(value(&frames, "CAN_HCO[5].out2"), Some(1.0));
        assert_eq!(value(&frames, "CAN_HCO[5].out2_pwm_us"), Some(2200.0));
        assert_eq!(value(&frames, "CAN_HCO[5].out4_pwm_us"), Some(2000.0));
        // A digitally driven output has no pulse width to report.
        assert!(!names(&frames).contains(&"CAN_HCO[5].out1_pwm_us".to_string()));
    }

    #[test]
    fn a_de_energized_output_reads_as_off() {
        let frames = [frame(0x245, [0; 8])];
        assert_eq!(value(&frames, "CAN_HCO[5].out1"), Some(0.0));
        assert!(!names(&frames).contains(&"CAN_HCO[5].out1_pwm_us".to_string()));
    }

    #[test]
    fn each_node_gets_its_own_series() {
        let frames = [frame(0x243, [0x00, 0x80, 0, 0, 0, 0, 0, 0]), frame(0x247, [0; 8])];
        assert_eq!(value(&frames, "CAN_HCO[3].out1"), Some(1.0));
        assert_eq!(value(&frames, "CAN_HCO[7].out1"), Some(0.0));
    }

    #[test]
    fn the_bus_joins_the_name_only_when_a_log_carries_more_than_one() {
        let mut second_bus = frame(0x243, [0; 8]);
        second_bus.bus = 2;
        assert_eq!(names(&[frame(0x243, [0; 8])])[0], "CAN_HCO[3].out1");
        let mixed = names(&[frame(0x243, [0; 8]), second_bus]);
        assert!(mixed.contains(&"CAN_HCO[bus1:3].out1".to_string()), "{mixed:?}");
        assert!(mixed.contains(&"CAN_HCO[bus2:3].out1".to_string()), "{mixed:?}");
    }

    /// Sensor slots are counts of whatever unit the node's `SensorUnits`
    /// frame declares, which is the only thing that says whether 1645 is
    /// 16.45 °C or 16.45 bar.
    #[test]
    fn sensor_slots_are_scaled_by_the_declared_unit() {
        let units = frame(0x2C5, [0x02, 0, 0, 0, 0, 0, 0, 0]);
        let values = frame(0x295, [0x6D, 0x06, 0x5D, 0x00, 0x70, 0x00, 0x64, 0x00]);
        let frames = [units, values];
        // Slot 0 declared centi-celsius, slots 1..3 left at centi-bar.
        assert_eq!(value(&frames, "CAN_SENSOR[5].slot0"), Some(16.45));
        assert_eq!(unit(&frames, "CAN_SENSOR[5].slot0").as_deref(), Some("°C"));
        assert_eq!(value(&frames, "CAN_SENSOR[5].slot1"), Some(0.93));
        assert_eq!(unit(&frames, "CAN_SENSOR[5].slot1").as_deref(), Some("bar"));
    }

    #[test]
    fn the_declared_unit_applies_to_samples_that_arrived_before_it() {
        let units = frame(0x2C5, [0x02, 0, 0, 0, 0, 0, 0, 0]);
        let mut early = frame(0x295, [0x6D, 0x06, 0, 0, 0, 0, 0, 0]);
        early.t_utc = -1.0;
        // The value frame is first on the wire; it still gets scaled.
        assert_eq!(value(&[early, units], "CAN_SENSOR[5].slot0"), Some(16.45));
    }

    #[test]
    fn unread_sensor_slots_and_amplifier_channels_are_gaps_not_values() {
        let sensors = frame(0x2A5, [0xBC, 0x11, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
        let adc = frame(0x255, [0xA8, 0x02, 0x0F, 0x00, 0x1A, 0x00, 0xFF, 0xFF]);
        let all = names(&[sensors, adc]);
        // Slot 4 read 4540 counts; slots 5..7 were the "no reading" sentinel.
        assert!(all.contains(&"CAN_SENSOR[5].slot4".to_string()), "{all:?}");
        assert!(!all.contains(&"CAN_SENSOR[5].slot5".to_string()), "{all:?}");
        assert!(all.contains(&"CAN_ADC[5].bus0_ch2".to_string()), "{all:?}");
        assert!(!all.contains(&"CAN_ADC[5].bus0_ch3".to_string()), "{all:?}");
    }

    #[test]
    fn a_valve_position_word_splits_into_position_and_drive_state() {
        // Valve 0 half open and held, valve 1 fully open but released.
        let mut data = [0u8; 8];
        data[..2].copy_from_slice(&500u16.to_le_bytes());
        data[2..4].copy_from_slice(&(1000u16 | VALVE_UNPOWERED).to_le_bytes());
        let frames = [frame(0x225, data)];
        assert_eq!(value(&frames, "CAN_VALVE[5].measured0"), Some(500.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].measured0_unpowered"), Some(0.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].measured1"), Some(1000.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].measured1_unpowered"), Some(1.0));
        assert_eq!(unit(&frames, "CAN_VALVE[5].measured0").as_deref(), Some(PROMILLE));
    }

    #[test]
    fn valve_status_and_ownership_unpack_from_their_nibbles() {
        // status = [3, 3, 0, 0], hco_owner = [1, 1, 2, 2], relief = 4.
        let frames = [frame(0x235, [0x33, 0x00, 0x11, 0x22, 0x04, 0, 0, 0])];
        assert_eq!(value(&frames, "CAN_VALVE[5].status0"), Some(3.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].status2"), Some(0.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].hco_owner3"), Some(2.0));
        assert_eq!(value(&frames, "CAN_VALVE[5].relief_state"), Some(4.0));
    }

    #[test]
    fn rails_status_and_heartbeat_decode() {
        let rails = frame(0x2E5, [0xD2, 0x27, 0x42, 0x21, 0x1A, 0x21, 0, 0]);
        let status = frame(0x305, [0x04, 0x00, 0x00, 0x00, 0x19, 0x4F, 0xF7, 0x01]);
        let mut heartbeat = frame(0x705, [0x05, 0, 0, 0, 0, 0, 0, 0]);
        heartbeat.len = 1;
        let frames = [rails, status, heartbeat];
        assert_eq!(value(&frames, "CAN_RAIL[5].voltage_logic"), Some(10194.0));
        assert_eq!(unit(&frames, "CAN_RAIL[5].voltage_logic").as_deref(), Some("mV"));
        assert_eq!(value(&frames, "CAN_RAIL[5].voltage_hco34"), Some(8474.0));
        assert_eq!(value(&frames, "CAN_STATUS[5].link_state"), Some(4.0));
        assert_eq!(value(&frames, "CAN_STATUS[5].ms_since_heartbeat"), Some(0x01F7_4F19 as f64));
        assert_eq!(value(&frames, "CAN_NODE[5].nmt_state"), Some(5.0));
        // The fourth word of a rail frame is padding, not a fourth rail.
        assert_eq!(names(&frames).iter().filter(|n| n.starts_with("CAN_RAIL")).count(), 3);
    }

    #[test]
    fn a_short_frame_on_a_process_data_identifier_is_not_decoded() {
        let mut short = frame(0x245, [0xFF; 8]);
        short.len = 4;
        assert!(decode(&[short]).is_empty());
    }
}
