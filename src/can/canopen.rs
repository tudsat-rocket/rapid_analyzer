//! The IO boards' object dictionary, and what a log says about it.
//!
//! The boards borrow CANopen's *framing* without its machinery: a node's
//! configuration and state live in an object dictionary (`index.sub`), read
//! and written one expedited SDO at a time, and most of the `0x2000` process
//! objects are also broadcast unasked as fixed TPDOs (see [`super::iocan`]).
//! So a log answers "what was object `0x2010.1` of node 5 at 12:03:41?" from
//! two directions:
//!
//! * **SDO traffic** -- `0x600 + node` requests, `0x580 + node` responses. A
//!   read response and a write request carry the value; a write
//!   acknowledgement and an abort only say that the access happened (and, for
//!   an abort, why it was refused).
//! * **TPDO mirrors** -- every process-data frame is one or more dictionary
//!   objects. This is the only source of values for a log that recorded the
//!   nodes' responses but not the master's requests, which is what the flight
//!   computer's own log looks like: it sees its own writes acknowledged, never
//!   the writes themselves.
//!
//! [`DICTIONARY`] mirrors `device-conf/can-io.toml` (and `src/store.rs` for the
//! generated `0x1000` objects) in the io board firmware repository. Like the
//! TPDO table, it is a copy: an object added there has to be added here, or it
//! shows up as "unknown object" with its raw bytes.

use std::collections::{BTreeMap, HashMap};

use super::CanFrame;
use super::iocan::{
    self, HCO_DIGITAL_OFF, HCO_DIGITAL_ON, HEARTBEAT_BASE, NODE_ID_MASK, SDO_REQUEST_BASE, SDO_RESPONSE_BASE, TpdoKind,
};
use crate::series::TimeSeries;

// ---------------------------------------------------------------------------
// The dictionary
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DataType {
    U8,
    U16,
    I16,
    U32,
    I32,
}

impl DataType {
    pub fn width(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
        }
    }

    /// The little-endian value in the first `width` bytes, sign-extended for
    /// the signed types.
    fn decode(self, bytes: [u8; 4]) -> i64 {
        match self {
            Self::U8 => bytes[0] as i64,
            Self::U16 => u16::from_le_bytes([bytes[0], bytes[1]]) as i64,
            Self::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as i64,
            Self::U32 => u32::from_le_bytes(bytes) as i64,
            Self::I32 => i32::from_le_bytes(bytes) as i64,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "ro",
            Self::ReadWrite => "rw",
        }
    }
}

/// What the entries of an array object are one of. Sub-index `n` is entry
/// `n - 1`; sub-index 0 of an array is its length, as CANopen has it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Element {
    Valve,
    /// Named the way the board silkscreen names them: HCO1..HCO4.
    Hco,
    Slot,
    Amplifier,
    I2cBus,
    Rail,
    Actuator,
    /// `0x3040`: one period per TPDO kind, in wire order.
    Tpdo,
    /// `0x1010` / `0x1011`: sub 1 is "all parameters".
    Parameters,
}

impl Element {
    fn label(self, entry: usize) -> String {
        match self {
            Self::Valve => format!("valve {entry}"),
            Self::Hco => format!("HCO{}", entry + 1),
            Self::Slot => format!("slot {entry}"),
            Self::Amplifier => format!("amp {entry}"),
            Self::I2cBus => format!("bus {entry}"),
            Self::Rail => ["logic", "hco12", "hco34"].get(entry).map_or_else(|| format!("rail {entry}"), |r| r.to_string()),
            Self::Actuator => format!("actuator {entry}"),
            Self::Tpdo => iocan::KINDS
                .get(entry)
                .map_or_else(|| format!("kind {entry}"), |k| k.name().to_string()),
            Self::Parameters => "all".to_string(),
        }
    }

    /// For a series name: short, and without spaces.
    fn tag(self, entry: usize) -> String {
        match self {
            Self::Hco => format!("hco{}", entry + 1),
            Self::Rail | Self::Tpdo => self.label(entry).to_lowercase(),
            _ => entry.to_string(),
        }
    }
}

/// How a raw value reads to a person.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Plain,
    /// The valve position word: promille in bits 0..14, "released" in bit 15.
    Position,
    /// Codes with names; anything past the end of the list is shown as a number.
    Enum(&'static [&'static str]),
    /// Which bits are set.
    Bits,
    /// `0xFF` means "none"; anything else is an index.
    IndexOrNone,
    /// `0x1010` / `0x1011`: the ASCII signatures `save` and `load`.
    Signature,
    /// A reading with a "nothing read" sentinel: an amplifier that did not
    /// answer, a sensor slot with no valid value.
    Reading(i64),
}

pub struct ObjectDef {
    pub index: u16,
    /// Series-name friendly, and what `src/store.rs` calls it.
    pub short: &'static str,
    /// What `can-io.toml` calls it.
    pub name: &'static str,
    pub ty: DataType,
    pub access: Access,
    /// `None` for a plain variable (sub-index 0 only).
    pub array: Option<(u8, Element)>,
    pub format: Format,
    pub unit: &'static str,
}

const fn var(index: u16, short: &'static str, name: &'static str, ty: DataType, access: Access) -> ObjectDef {
    ObjectDef {
        index,
        short,
        name,
        ty,
        access,
        array: None,
        format: Format::Plain,
        unit: "",
    }
}

impl ObjectDef {
    const fn array(mut self, len: u8, element: Element) -> Self {
        self.array = Some((len, element));
        self
    }

    const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    const fn format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// The sub-indices that carry a value: `0` for a variable, `1..=len` for
    /// an array.
    pub fn subs(&self) -> std::ops::RangeInclusive<u8> {
        match self.array {
            Some((len, _)) => 1..=len,
            None => 0..=0,
        }
    }

    /// What a sub-index is an entry *of*: `"valve 0"`, `"HCO3"`, `"logic"`.
    pub fn sub_label(&self, sub: u8) -> Option<String> {
        let (_, element) = self.array?;
        if sub == 0 {
            return Some("length".to_string());
        }
        Some(element.label(sub as usize - 1))
    }

    /// The raw value, formatted the way the firmware documents it.
    pub fn format_value(&self, raw: i64) -> String {
        let with_unit = |n: String| {
            if self.unit.is_empty() {
                n
            } else {
                format!("{n} {}", self.unit)
            }
        };
        match self.format {
            Format::Plain => with_unit(raw.to_string()),
            Format::Position => {
                let word = raw as u16;
                let promille = word & 0x7FFF;
                if word & 0x8000 != 0 {
                    format!("{promille} ‰, released")
                } else {
                    format!("{promille} ‰")
                }
            }
            Format::Enum(labels) => match usize::try_from(raw).ok().and_then(|i| labels.get(i)) {
                Some(label) => format!("{raw} {label}"),
                None => format!("{raw} (unknown code)"),
            },
            Format::Bits => {
                let set: Vec<String> = (0..self.ty.width() * 8)
                    .filter(|bit| (raw >> bit) & 1 == 1)
                    .map(|bit| bit.to_string())
                    .collect();
                if set.is_empty() {
                    "none set".to_string()
                } else {
                    format!("bits {}", set.join(", "))
                }
            }
            Format::IndexOrNone => {
                if raw == 0xFF {
                    "none".to_string()
                } else {
                    raw.to_string()
                }
            }
            Format::Reading(invalid) if raw == invalid => "no reading".to_string(),
            Format::Reading(_) => with_unit(raw.to_string()),
            Format::Signature => match raw as u32 {
                SIGNATURE_SAVE => "\"save\"".to_string(),
                SIGNATURE_LOAD => "\"load\"".to_string(),
                other => format!("0x{other:08X}"),
            },
        }
    }

    /// The number a graph of this object draws. The only object whose raw
    /// value is not already the number is the position word, whose top bit is
    /// a flag -- plotted raw, a released valve would read as 33 000 ‰.
    pub fn plot_value(&self, raw: i64) -> f64 {
        match self.format {
            Format::Position => (raw & 0x7FFF) as f64,
            _ => raw as f64,
        }
    }

    fn plot_unit(&self) -> Option<String> {
        match self.format {
            Format::Position => Some("‰".to_string()),
            _ if !self.unit.is_empty() => Some(self.unit.to_string()),
            _ => None,
        }
    }
}

const SIGNATURE_SAVE: u32 = 0x6576_6173;
const SIGNATURE_LOAD: u32 = 0x6461_6F6C;

pub const VALVE_STATUS: &[&str] = &["unmapped", "unpowered", "moving", "holding", "stalled"];
/// 4 is raw debug mode *or* the fallback switched off at 0x3007 -- see
/// `safety::evaluate` in the firmware.
pub const LINK_STATE: &[&str] = &[
    "never seen master",
    "alive",
    "fallback A",
    "fallback B",
    "suspended (raw debug or fallback off)",
];
pub const RELIEF_STATE: &[&str] = &["idle", "relieving", "cooldown", "inhibited", "disabled"];
const UNIT_CODE: &[&str] = &["centibar", "decibar", "centi-°C", "raw counts"];
const VALVE_KIND: &[&str] = &["not fitted", "solenoid", "servo", "stepper"];
const SENSOR_KIND: &[&str] = &["unused", "pressure (linear)", "Pt1000 temperature"];
const HCO_NUMBER: &[&str] = &["none", "HCO1", "HCO2", "HCO3", "HCO4"];
const HCO_OWNER: &[&str] = &["free", "valve 0", "valve 1", "valve 2", "valve 3"];
const OFF_ON: &[&str] = &["off", "on"];

use Access::{ReadOnly as RO, ReadWrite as RW};
use DataType::{I16, I32, U8, U16, U32};

/// Every object a node answers for, sorted by index.
pub const DICTIONARY: &[ObjectDef] = &[
    // --- 0x1000: generated from `support_storage` and `heartbeat_period` ---
    var(0x1010, "store_parameters", "store parameters (write \"save\")", U32, RW)
        .array(1, Element::Parameters)
        .format(Format::Signature),
    var(0x1011, "restore_defaults", "restore default parameters (write \"load\")", U32, RW)
        .array(1, Element::Parameters)
        .format(Format::Signature),
    var(0x1017, "heartbeat_period", "producer heartbeat period", U16, RW).unit("ms"),
    // --- 0x2000: process data ---
    var(0x2000, "raw_adc_bus0", "raw external adc reading, i2c bus 0", U16, RO)
        .array(9, Element::Amplifier)
        .format(Format::Reading(0xFFFF))
        .unit("counts"),
    var(0x2001, "raw_adc_bus1", "raw external adc reading, i2c bus 1", U16, RO)
        .array(9, Element::Amplifier)
        .format(Format::Reading(0xFFFF))
        .unit("counts"),
    var(0x2002, "i2c_present", "i2c amplifier presence bitmap", U16, RO)
        .array(2, Element::I2cBus)
        .format(Format::Bits),
    var(0x2003, "i2c_sweeps", "completed i2c presence sweeps", U32, RO),
    var(0x2004, "sensor_value", "calibrated sensor values (unit: 0x2005)", I16, RO)
        .array(8, Element::Slot)
        .format(Format::Reading(i16::MIN as i64))
        .unit("counts"),
    var(0x2005, "sensor_unit", "unit code per sensor slot", U8, RO)
        .array(8, Element::Slot)
        .format(Format::Enum(UNIT_CODE)),
    var(0x2010, "valve_commanded", "valve commanded state", U16, RW)
        .array(4, Element::Valve)
        .format(Format::Position),
    var(0x2011, "valve_target", "valve target state", U16, RO)
        .array(4, Element::Valve)
        .format(Format::Position),
    var(0x2012, "valve_measured", "valve measured state", U16, RO)
        .array(4, Element::Valve)
        .format(Format::Position),
    var(0x2013, "valve_status", "valve status", U8, RO)
        .array(4, Element::Valve)
        .format(Format::Enum(VALVE_STATUS)),
    var(0x2014, "valve_current", "valve current", U16, RO)
        .array(4, Element::Valve)
        .unit("mA"),
    var(0x2015, "relief_state", "overpressure relief state", U8, RO).format(Format::Enum(RELIEF_STATE)),
    var(0x2016, "stepper_position", "stepper position, steps since boot", I32, RW)
        .array(2, Element::Actuator)
        .unit("steps"),
    var(0x2020, "hco_digital", "high current output, digital level", U8, RW)
        .array(4, Element::Hco)
        .format(Format::Enum(OFF_ON)),
    var(0x2021, "hco_pwm", "high current output, pwm pulse width", U16, RW)
        .array(4, Element::Hco)
        .unit("us"),
    var(0x2022, "hco_owner", "high current output owner", U8, RO)
        .array(4, Element::Hco)
        .format(Format::Enum(HCO_OWNER)),
    var(0x2030, "leds", "debug leds (bit0 red, bit1 yellow, bit2 white)", U8, RW).format(Format::Bits),
    var(0x2031, "raw_debug", "raw debug mode (volatile)", U8, RW).format(Format::Enum(OFF_ON)),
    var(0x2032, "link_state", "master link state", U8, RO).format(Format::Enum(LINK_STATE)),
    var(0x2033, "ms_since_heartbeat", "time since last master heartbeat", U32, RO).unit("ms"),
    var(0x2040, "rail_current", "rail current", U16, RO)
        .array(3, Element::Rail)
        .unit("mA"),
    var(0x2041, "rail_voltage", "rail voltage", U16, RO)
        .array(3, Element::Rail)
        .unit("mV"),
    // --- 0x3000: runtime configuration, persisted on "save" ---
    var(0x3000, "master_node_id", "master node id", U8, RW),
    var(0x3001, "fallback_a_ms", "fallback stage A timeout", U32, RW).unit("ms"),
    var(0x3002, "fallback_b_ms", "fallback stage B timeout", U32, RW).unit("ms"),
    var(0x3003, "fallback_a_position", "fallback stage A valve position", U16, RW)
        .array(4, Element::Valve)
        .unit("‰"),
    var(0x3004, "fallback_b_position", "fallback stage B valve position", U16, RW)
        .array(4, Element::Valve)
        .unit("‰"),
    var(0x3005, "fallback_a_unpower", "unpower servo after fallback stage A", U8, RW)
        .array(4, Element::Valve)
        .format(Format::Enum(OFF_ON)),
    var(0x3006, "fallback_b_unpower", "unpower servo after fallback stage B", U8, RW)
        .array(4, Element::Valve)
        .format(Format::Enum(OFF_ON)),
    var(0x3007, "fallback_enabled", "heartbeat fallback enabled", U8, RW).format(Format::Enum(OFF_ON)),
    var(0x3010, "valve_kind", "valve kind", U8, RW)
        .array(4, Element::Valve)
        .format(Format::Enum(VALVE_KIND)),
    var(0x3011, "valve_power_hco", "valve power output", U8, RW)
        .array(4, Element::Valve)
        .format(Format::Enum(HCO_NUMBER)),
    var(0x3012, "valve_signal_hco", "valve signal output", U8, RW)
        .array(4, Element::Valve)
        .format(Format::Enum(HCO_NUMBER)),
    var(0x3013, "valve_closed_us", "servo pulse width at fully closed", U16, RW)
        .array(4, Element::Valve)
        .unit("us"),
    var(0x3014, "valve_open_us", "servo pulse width at fully open", U16, RW)
        .array(4, Element::Valve)
        .unit("us"),
    var(0x3015, "valve_travel_ms", "servo full travel time", U16, RW)
        .array(4, Element::Valve)
        .unit("ms"),
    var(0x3016, "valve_stall_ma", "valve stall current threshold (0 = off)", U16, RW)
        .array(4, Element::Valve)
        .unit("mA"),
    var(0x3017, "valve_stall_ms", "valve stall detect debounce", U16, RW)
        .array(4, Element::Valve)
        .unit("ms"),
    var(0x3018, "valve_settle_ms", "valve settle time after arriving", U16, RW)
        .array(4, Element::Valve)
        .unit("ms"),
    var(0x3019, "valve_min_promille", "valve minimum commandable position", U16, RW)
        .array(4, Element::Valve)
        .unit("‰"),
    var(0x301A, "valve_max_promille", "valve maximum commandable position", U16, RW)
        .array(4, Element::Valve)
        .unit("‰"),
    var(0x3020, "sensor_bus", "sensor slot source bus (0xFF = unused)", U8, RW)
        .array(8, Element::Slot)
        .format(Format::IndexOrNone),
    var(0x3021, "sensor_amplifier", "sensor slot source amplifier index", U8, RW).array(8, Element::Slot),
    var(0x3022, "sensor_kind", "sensor slot kind", U8, RW)
        .array(8, Element::Slot)
        .format(Format::Enum(SENSOR_KIND)),
    var(0x3023, "sensor_offset", "sensor calibration offset", I32, RW)
        .array(8, Element::Slot)
        .unit("milli-counts"),
    var(0x3024, "sensor_slope", "sensor calibration slope", I32, RW)
        .array(8, Element::Slot)
        .unit("nbar/count"),
    var(0x3025, "sensor_unit_cfg", "sensor slot unit code", U8, RW)
        .array(8, Element::Slot)
        .format(Format::Enum(UNIT_CODE)),
    var(0x3026, "sensor_constant", "sensor calibration constant term", I32, RW)
        .array(8, Element::Slot)
        .unit("mbar"),
    var(0x3030, "sensor_interval_ms", "sensor sample interval", U16, RW).unit("ms"),
    var(0x3031, "scan_interval_ms", "i2c presence probe interval", U16, RW).unit("ms"),
    var(0x3040, "tpdo_interval_ms", "tpdo broadcast period per kind (0 = off)", U16, RW)
        .array(18, Element::Tpdo)
        .unit("ms"),
    var(0x3050, "relief_enabled", "overpressure relief enabled", U8, RW).format(Format::Enum(OFF_ON)),
    var(0x3051, "relief_valve", "overpressure relief valve index", U8, RW).format(Format::IndexOrNone),
    var(0x3052, "relief_sensor", "overpressure relief sensor slot", U8, RW),
    var(0x3053, "relief_threshold", "overpressure relief threshold (slot's own unit)", I16, RW).unit("counts"),
    var(0x3054, "relief_position", "overpressure relief valve position", U16, RW).unit("‰"),
    var(0x3055, "relief_pulse_ms", "overpressure relief pulse length", U16, RW).unit("ms"),
    var(0x3056, "relief_cooldown_ms", "overpressure relief cooldown", U16, RW).unit("ms"),
    var(0x3060, "stepper_valve", "stepper valve index", U8, RW)
        .array(2, Element::Actuator)
        .format(Format::IndexOrNone),
    var(0x3061, "stepper_closed_steps", "stepper step count at fully closed", I32, RW)
        .array(2, Element::Actuator)
        .unit("steps"),
    var(0x3062, "stepper_open_steps", "stepper step count at fully open", I32, RW)
        .array(2, Element::Actuator)
        .unit("steps"),
    var(0x3063, "stepper_max_hz", "stepper traverse speed", U32, RW)
        .array(2, Element::Actuator)
        .unit("steps/s"),
    var(0x3064, "stepper_start_hz", "stepper pull-in rate", U32, RW)
        .array(2, Element::Actuator)
        .unit("steps/s"),
    var(0x3065, "stepper_accel", "stepper acceleration", U32, RW)
        .array(2, Element::Actuator)
        .unit("steps/s²"),
];

/// The definition of an object, if the firmware has one at that index.
pub fn object(index: u16) -> Option<&'static ObjectDef> {
    DICTIONARY
        .binary_search_by_key(&index, |def| def.index)
        .ok()
        .map(|i| &DICTIONARY[i])
}

/// `0x2010.1 valve commanded state [valve 0]`, or the bare address for an
/// object the dictionary doesn't know.
pub fn describe_object(index: u16, sub: u8) -> String {
    match object(index) {
        Some(def) => match def.sub_label(sub) {
            Some(entry) => format!("0x{index:04X}.{sub} {} [{entry}]", def.name),
            None => format!("0x{index:04X}.{sub} {}", def.name),
        },
        None => format!("0x{index:04X}.{sub} unknown object"),
    }
}

/// The CiA 301 abort codes, with the firmware's reading where it uses one
/// for something specific.
pub fn abort_reason(code: u32) -> &'static str {
    match code {
        0x0503_0000 => "toggle bit not alternated",
        0x0504_0000 => "SDO protocol timed out",
        0x0504_0001 => "command specifier not valid or unknown",
        0x0504_0002 => "invalid block size",
        0x0504_0003 => "invalid sequence number",
        0x0504_0004 => "CRC error",
        0x0504_0005 => "out of memory",
        0x0601_0000 => "unsupported access (only expedited transfers are)",
        0x0601_0001 => "attempt to read a write-only object",
        0x0601_0002 => "attempt to write a read-only object",
        0x0602_0000 => "object does not exist",
        0x0604_0041 => "object cannot be mapped to a PDO",
        0x0604_0042 => "PDO length exceeded",
        0x0604_0043 => "general parameter incompatibility",
        0x0604_0047 => "general internal incompatibility",
        0x0606_0000 => "hardware error",
        0x0607_0010 => "data type does not match (size not given)",
        0x0607_0012 => "data type does not match, too long",
        0x0607_0013 => "data type does not match, too short",
        0x0609_0011 => "sub-index does not exist",
        0x0609_0030 => "invalid value",
        0x0609_0031 => "value too high",
        0x0609_0032 => "value too low",
        0x0609_0036 => "maximum less than minimum",
        0x060A_0023 => "resource not available",
        0x0800_0000 => "general error",
        0x0800_0020 => "cannot store (rejected on save)",
        0x0800_0021 => "local control: output owned by a valve (raw debug 0x2031 overrides)",
        0x0800_0022 => "not possible in the present device state",
        0x0800_0023 => "no object dictionary",
        0x0800_0024 => "no data available",
        _ => "unknown abort code",
    }
}

/// The NMT state a heartbeat carries.
pub fn nmt_label(state: u8) -> &'static str {
    match state {
        0x00 => "boot-up",
        0x04 => "stopped",
        0x05 => "operational",
        0x7F => "pre-operational",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// SDO frames
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SdoKind {
    /// Client asks for a value: initiate upload.
    ReadRequest,
    /// Client writes a value: expedited initiate download.
    WriteRequest,
    /// Node answers a read with the value: expedited initiate upload response.
    ReadResponse,
    /// Node accepted a write. Carries no value.
    WriteAck,
    /// Either side gave up; the payload is the reason.
    Abort(u32),
    /// Segmented or block transfer, which these nodes refuse -- kept so the
    /// attempt is visible, with its command byte.
    Other(u8),
}

impl SdoKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadRequest => "read",
            Self::WriteRequest => "write",
            Self::ReadResponse => "read ok",
            Self::WriteAck => "write ok",
            Self::Abort(_) => "ABORT",
            Self::Other(_) => "other",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SdoTransfer {
    pub t_utc: f64,
    pub bus: u8,
    pub node: u8,
    /// Client to node (`0x600 + node`) rather than node to client.
    pub request: bool,
    pub kind: SdoKind,
    pub index: u16,
    pub sub: u8,
    pub data: [u8; 4],
    /// Payload bytes, when the frame says.
    pub size: Option<u8>,
    /// For a response: how long after its request it came, if the log has
    /// the request.
    pub latency: Option<f64>,
}

impl SdoTransfer {
    /// The value carried, if this kind of frame carries one.
    pub fn value(&self) -> Option<i64> {
        if !matches!(self.kind, SdoKind::WriteRequest | SdoKind::ReadResponse) {
            return None;
        }
        let ty = object(self.index).map(|def| def.ty);
        Some(match (ty, self.size) {
            (Some(ty), _) => ty.decode(self.data),
            (None, Some(1)) => DataType::U8.decode(self.data),
            (None, Some(2)) => DataType::U16.decode(self.data),
            (None, _) => DataType::U32.decode(self.data),
        })
    }

    /// The value, or what else there is to say about the frame.
    pub fn detail(&self) -> String {
        match self.kind {
            SdoKind::Abort(code) => format!("0x{code:08X} {}", abort_reason(code)),
            SdoKind::Other(cmd) => format!("command byte 0x{cmd:02X} (segmented/block, unsupported)"),
            _ => match (self.value(), object(self.index)) {
                (Some(raw), Some(def)) => def.format_value(raw),
                (Some(raw), None) => format!("{raw} (0x{raw:X})"),
                (None, _) => String::new(),
            },
        }
    }
}

/// Reads one SDO frame, or `None` for anything that isn't one.
pub fn decode_sdo(frame: &CanFrame) -> Option<SdoTransfer> {
    let base = frame.id & !NODE_ID_MASK;
    let request = match base {
        SDO_REQUEST_BASE => true,
        SDO_RESPONSE_BASE => false,
        _ => return None,
    };
    if frame.len < 4 {
        return None;
    }
    let cmd = frame.data[0];
    let expedited = cmd & 0x02 != 0;
    let size_given = cmd & 0x01 != 0;
    let unused = (cmd >> 2) & 0x03;
    let kind = match (request, cmd >> 5) {
        (true, 1) if expedited => SdoKind::WriteRequest,
        (true, 2) => SdoKind::ReadRequest,
        (false, 2) if expedited => SdoKind::ReadResponse,
        (false, 3) => SdoKind::WriteAck,
        (_, 4) => SdoKind::Abort(u32::from_le_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]])),
        _ => SdoKind::Other(cmd),
    };
    let size = (matches!(kind, SdoKind::WriteRequest | SdoKind::ReadResponse) && size_given).then_some(4 - unused);
    Some(SdoTransfer {
        t_utc: frame.t_utc,
        bus: frame.bus,
        node: (frame.id & NODE_ID_MASK) as u8,
        request,
        kind,
        index: u16::from_le_bytes([frame.data[1], frame.data[2]]),
        sub: frame.data[3],
        data: [frame.data[4], frame.data[5], frame.data[6], frame.data[7]],
        size,
        latency: None,
    })
}

// ---------------------------------------------------------------------------
// TPDO mirrors
// ---------------------------------------------------------------------------

/// Every dictionary object a TPDO frame carries, as `(index, sub, raw)`.
///
/// This is the other half of [`super::iocan`]: there the frame becomes named
/// series, here it becomes the dictionary entries the firmware documents it
/// as a copy of (`iocan-proto/src/tpdo.rs` says which, per kind).
fn tpdo_objects(kind: TpdoKind, data: &[u8; 8], mut emit: impl FnMut(u16, u8, i64)) {
    let words = iocan::u16x4(data);
    let array = |emit: &mut dyn FnMut(u16, u8, i64), index: u16, first: u8, values: &[i64]| {
        for (i, v) in values.iter().enumerate() {
            emit(index, first + i as u8, *v);
        }
    };
    let w = words.map(|w| w as i64);
    match kind {
        TpdoKind::ValveCommanded => array(&mut emit, 0x2010, 1, &w),
        TpdoKind::ValveTarget => array(&mut emit, 0x2011, 1, &w),
        TpdoKind::ValveMeasured => array(&mut emit, 0x2012, 1, &w),
        TpdoKind::ValveCurrent => array(&mut emit, 0x2014, 1, &w),
        TpdoKind::ValveStatus => {
            array(&mut emit, 0x2013, 1, &iocan::unpack_nibbles(&data[..2]).map(|v| v as i64));
            array(&mut emit, 0x2022, 1, &iocan::unpack_nibbles(&data[2..4]).map(|v| v as i64));
            emit(0x2015, 0, data[4] as i64);
        }
        TpdoKind::HcoState => {
            for (i, word) in words.into_iter().enumerate() {
                let sub = i as u8 + 1;
                match word {
                    HCO_DIGITAL_ON => emit(0x2020, sub, 1),
                    HCO_DIGITAL_OFF => {
                        emit(0x2020, sub, 0);
                        emit(0x2021, sub, 0);
                    }
                    us => emit(0x2021, sub, us as i64),
                }
            }
        }
        TpdoKind::RawBus0A => array(&mut emit, 0x2000, 1, &w),
        TpdoKind::RawBus0B => array(&mut emit, 0x2000, 5, &w),
        TpdoKind::RawBus1A => array(&mut emit, 0x2001, 1, &w),
        TpdoKind::RawBus1B => array(&mut emit, 0x2001, 5, &w),
        TpdoKind::Sensor0 | TpdoKind::Sensor1 | TpdoKind::Sensor3 => {
            let first = match kind {
                TpdoKind::Sensor0 => 1,
                TpdoKind::Sensor1 => 5,
                _ => 9,
            };
            array(&mut emit, 0x2004, first, &iocan::i16x4(data).map(|v| v as i64));
        }
        TpdoKind::SensorUnits => {
            let codes = iocan::unpack_2bit(&data[..3]);
            array(&mut emit, 0x2005, 1, &codes.map(|c| c as i64));
        }
        TpdoKind::I2cScan => {
            array(&mut emit, 0x2002, 1, &w[..2]);
            // Only the low 16 bits of the counter travel.
            emit(0x2003, 0, w[2]);
        }
        TpdoKind::RailVoltage => array(&mut emit, 0x2041, 1, &w[..3]),
        TpdoKind::RailCurrent => array(&mut emit, 0x2040, 1, &w[..3]),
        TpdoKind::Status => {
            emit(0x2032, 0, data[0] as i64);
            emit(0x2031, 0, (data[1] != 0) as i64);
            emit(0x2033, 0, u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as i64);
        }
    }
}

// ---------------------------------------------------------------------------
// The log, read as a dictionary over time
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NodeKey {
    pub bus: u8,
    pub node: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ObjectKey {
    pub bus: u8,
    pub node: u8,
    pub index: u16,
    pub sub: u8,
}

impl ObjectKey {
    pub fn node(&self) -> NodeKey {
        NodeKey {
            bus: self.bus,
            node: self.node,
        }
    }
}

/// Where a value came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Tpdo,
    SdoRead,
    SdoWrite,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tpdo => "TPDO",
            Self::SdoRead => "SDO read",
            Self::SdoWrite => "SDO write",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ValueChange {
    pub t_utc: f64,
    pub raw: i64,
    pub origin: Origin,
}

/// One object of one node, across the log.
#[derive(Default)]
pub struct ObjectTrack {
    /// Only the samples where the value *changed* -- a TPDO repeating the same
    /// number twice a second for fifteen minutes is one entry, not 1800.
    pub changes: Vec<ValueChange>,
    /// Every time a value was seen, changed or not.
    pub updates: usize,
    pub last_seen: f64,
    pub reads: usize,
    pub writes: usize,
    pub aborts: usize,
    pub last_abort: Option<u32>,
}

impl ObjectTrack {
    /// The value in force at `t`: the last change at or before it.
    pub fn value_at(&self, t_utc: f64) -> Option<&ValueChange> {
        let i = self.changes.partition_point(|c| c.t_utc <= t_utc);
        i.checked_sub(1).map(|i| &self.changes[i])
    }

    fn record(&mut self, t_utc: f64, raw: i64, origin: Origin) {
        self.updates += 1;
        self.last_seen = t_utc;
        if self.changes.last().is_none_or(|c| c.raw != raw) {
            self.changes.push(ValueChange { t_utc, raw, origin });
        }
    }
}

/// What one node did over the log.
pub struct NodeInfo {
    pub key: NodeKey,
    pub first_seen: f64,
    pub last_seen: f64,
    pub heartbeats: usize,
    /// First and last heartbeat -- not first and last frame, or a node whose
    /// TPDOs start before its heartbeat would seem to beat slower than it does.
    heartbeat_span: Option<(f64, f64)>,
    pub nmt_state: Option<u8>,
    /// The longest silence between two heartbeats, and when it ended.
    pub longest_heartbeat_gap: Option<(f64, f64)>,
    /// Frames per TPDO kind, by wire index.
    pub tpdo_frames: BTreeMap<usize, usize>,
    pub sdo_requests: usize,
    pub sdo_responses: usize,
    pub aborts: usize,
}

impl NodeInfo {
    fn new(key: NodeKey, t: f64) -> Self {
        Self {
            key,
            first_seen: t,
            last_seen: t,
            heartbeats: 0,
            heartbeat_span: None,
            nmt_state: None,
            longest_heartbeat_gap: None,
            tpdo_frames: BTreeMap::new(),
            sdo_requests: 0,
            sdo_responses: 0,
            aborts: 0,
        }
    }

    /// Mean heartbeat period, the thing `0x1017` configures.
    pub fn heartbeat_period(&self) -> Option<f64> {
        let (first, last) = self.heartbeat_span?;
        (self.heartbeats > 1).then(|| (last - first) / (self.heartbeats - 1) as f64)
    }
}

/// Everything the log says about the IO boards' dictionaries.
#[derive(Default)]
pub struct CanOpenLog {
    pub transfers: Vec<SdoTransfer>,
    pub nodes: Vec<NodeInfo>,
    pub objects: BTreeMap<ObjectKey, ObjectTrack>,
    pub multi_bus: bool,
}

impl CanOpenLog {
    pub fn build(frames: &[CanFrame]) -> Self {
        let mut log = Self::default();
        let mut nodes: BTreeMap<NodeKey, NodeInfo> = BTreeMap::new();
        let mut last_heartbeat: HashMap<NodeKey, f64> = HashMap::new();
        // The request each node is still answering. Only one is outstanding
        // at a time: expedited SDO is strictly request, response.
        let mut pending: HashMap<NodeKey, usize> = HashMap::new();
        let mut buses: Vec<u8> = Vec::new();

        for frame in frames {
            if frame.id > 0x7FF {
                continue;
            }
            let key = NodeKey {
                bus: frame.bus,
                node: (frame.id & NODE_ID_MASK) as u8,
            };
            let object_key = |index: u16, sub: u8| ObjectKey {
                bus: key.bus,
                node: key.node,
                index,
                sub,
            };

            if let Some(mut transfer) = decode_sdo(frame) {
                let info = nodes.entry(key).or_insert_with(|| NodeInfo::new(key, frame.t_utc));
                info.last_seen = frame.t_utc;
                if transfer.request {
                    info.sdo_requests += 1;
                    // A request that was never answered still happened: a
                    // write that was sent is the best guess at the value.
                    if let Some(stale) = pending.insert(key, log.transfers.len()) {
                        apply_unanswered(&mut log.objects, &log.transfers[stale]);
                    }
                    log.transfers.push(transfer);
                } else {
                    info.sdo_responses += 1;
                    let request = pending
                        .remove(&key)
                        .map(|i| log.transfers[i])
                        .filter(|r| (r.index, r.sub) == (transfer.index, transfer.sub));
                    transfer.latency = request.map(|r| transfer.t_utc - r.t_utc);
                    let track = log.objects.entry(object_key(transfer.index, transfer.sub)).or_default();
                    match transfer.kind {
                        SdoKind::ReadResponse => {
                            track.reads += 1;
                            if let Some(raw) = transfer.value() {
                                track.record(transfer.t_utc, raw, Origin::SdoRead);
                            }
                        }
                        SdoKind::WriteAck => {
                            track.writes += 1;
                            // The value is in the request; only an
                            // acknowledged write is known to have landed.
                            if let Some(raw) = request.and_then(|r| r.value()) {
                                track.record(transfer.t_utc, raw, Origin::SdoWrite);
                            }
                        }
                        SdoKind::Abort(code) => {
                            track.aborts += 1;
                            track.last_abort = Some(code);
                            info.aborts += 1;
                        }
                        _ => {}
                    }
                    log.transfers.push(transfer);
                }
                if !buses.contains(&frame.bus) {
                    buses.push(frame.bus);
                }
                continue;
            }

            if (HEARTBEAT_BASE..HEARTBEAT_BASE + 16).contains(&frame.id) {
                let info = nodes.entry(key).or_insert_with(|| NodeInfo::new(key, frame.t_utc));
                info.last_seen = frame.t_utc;
                info.heartbeats += 1;
                let first = info.heartbeat_span.map_or(frame.t_utc, |(first, _)| first);
                info.heartbeat_span = Some((first, frame.t_utc));
                if frame.len >= 1 {
                    info.nmt_state = Some(frame.data[0]);
                }
                if let Some(previous) = last_heartbeat.insert(key, frame.t_utc) {
                    let gap = frame.t_utc - previous;
                    if info.longest_heartbeat_gap.is_none_or(|(longest, _)| gap > longest) {
                        info.longest_heartbeat_gap = Some((gap, frame.t_utc));
                    }
                }
                if !buses.contains(&frame.bus) {
                    buses.push(frame.bus);
                }
                continue;
            }

            if let Some((_, kind)) = iocan::process_data(frame.id)
                && frame.len == 8
            {
                let info = nodes.entry(key).or_insert_with(|| NodeInfo::new(key, frame.t_utc));
                info.last_seen = frame.t_utc;
                *info.tpdo_frames.entry(kind as usize).or_default() += 1;
                tpdo_objects(kind, &frame.data, |index, sub, raw| {
                    log.objects
                        .entry(object_key(index, sub))
                        .or_default()
                        .record(frame.t_utc, raw, Origin::Tpdo);
                });
                if !buses.contains(&frame.bus) {
                    buses.push(frame.bus);
                }
            }
        }
        for (_, i) in pending {
            apply_unanswered(&mut log.objects, &log.transfers[i]);
        }

        // The TPDO mirrors were interleaved with SDO traffic in time, but a
        // write applied late (unanswered) can land out of order.
        for track in log.objects.values_mut() {
            track.changes.sort_by(|a, b| a.t_utc.total_cmp(&b.t_utc));
        }
        log.nodes = nodes.into_values().collect();
        log.multi_bus = buses.len() > 1;
        log
    }

    pub fn node(&self, key: NodeKey) -> Option<&NodeInfo> {
        self.nodes.iter().find(|n| n.key == key)
    }

    /// Every object of one node the log has anything about, in index order.
    pub fn objects_of(&self, node: NodeKey) -> impl Iterator<Item = (&ObjectKey, &ObjectTrack)> {
        let lo = ObjectKey {
            bus: node.bus,
            node: node.node,
            index: 0,
            sub: 0,
        };
        let hi = ObjectKey {
            index: u16::MAX,
            sub: u8::MAX,
            ..lo
        };
        self.objects.range(lo..=hi)
    }

    pub fn value_at(&self, key: ObjectKey, t_utc: f64) -> Option<&ValueChange> {
        self.objects.get(&key)?.value_at(t_utc)
    }

    /// `raw` as a person reads it, knowing what else the node said at `t`: a
    /// sensor slot's counts are only a quantity once its unit code (0x2005,
    /// same slot) says which.
    pub fn format_value_at(&self, key: ObjectKey, raw: i64, t_utc: f64) -> String {
        let Some(def) = object(key.index) else {
            return raw.to_string();
        };
        let text = def.format_value(raw);
        if key.index != 0x2004 || raw == i16::MIN as i64 {
            return text;
        }
        let code = self
            .value_at(ObjectKey { index: 0x2005, ..key }, t_utc)
            .and_then(|v| u8::try_from(v.raw).ok());
        match iocan::sensor_unit(code) {
            (unit, scale) if scale != 1.0 => {
                // As many decimals as the scale has -- centibar is 0.01 bar.
                let decimals = (-scale.log10()).round().max(0.0) as usize;
                format!("{text} = {:.decimals$} {unit}", raw as f64 * scale)
            }
            _ => text,
        }
    }

    /// Whether the master's requests were recorded at all. The flight
    /// computer's log has only the nodes' answers, so a write is known to have
    /// happened but not what it wrote.
    pub fn has_requests(&self) -> bool {
        self.transfers.iter().any(|t| t.request)
    }

    /// One object's history as a series, drawn as the steps it really is.
    pub fn series(&self, key: ObjectKey) -> Option<TimeSeries> {
        let track = self.objects.get(&key)?;
        let first = track.changes.first()?;
        let def = object(key.index);
        let value = |raw: i64| def.map_or(raw as f64, |d| d.plot_value(raw));
        let mut points = Vec::with_capacity(track.changes.len() * 2 + 1);
        points.push([first.t_utc, value(first.raw)]);
        for pair in track.changes.windows(2) {
            // Hold the old value up to the change, so the line steps rather
            // than ramping between two samples that may be minutes apart.
            points.push([pair[1].t_utc, value(pair[0].raw)]);
            points.push([pair[1].t_utc, value(pair[1].raw)]);
        }
        let last = track.changes.last().map_or(first.raw, |c| c.raw);
        if track.last_seen > points.last().map_or(f64::MIN, |p| p[0]) {
            points.push([track.last_seen, value(last)]);
        }
        let unit = def.and_then(|d| d.plot_unit());
        Some(TimeSeries::from_points(self.series_name(key), points).with_unit(unit))
    }

    /// `CAN_OD[5].valve_commanded_0`, in the style of [`super::iocan`]'s names.
    pub fn series_name(&self, key: ObjectKey) -> String {
        let instance = if self.multi_bus {
            format!("bus{}:{}", key.bus, key.node)
        } else {
            key.node.to_string()
        };
        let field = match object(key.index) {
            Some(def) => match def.array {
                Some((_, element)) if key.sub > 0 => format!("{}_{}", def.short, element.tag(key.sub as usize - 1)),
                _ => def.short.to_string(),
            },
            None => format!("x{:04X}_{}", key.index, key.sub),
        };
        format!("CAN_OD[{instance}].{field}")
    }
}

fn apply_unanswered(objects: &mut BTreeMap<ObjectKey, ObjectTrack>, request: &SdoTransfer) {
    if request.kind != SdoKind::WriteRequest {
        return;
    }
    if let Some(raw) = request.value() {
        objects
            .entry(ObjectKey {
                bus: request.bus,
                node: request.node,
                index: request.index,
                sub: request.sub,
            })
            .or_default()
            .record(request.t_utc, raw, Origin::SdoWrite);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f64, id: u32, data: [u8; 8]) -> CanFrame {
        CanFrame {
            t_utc: t,
            id,
            bus: 1,
            len: 8,
            data,
        }
    }

    fn key(node: u8, index: u16, sub: u8) -> ObjectKey {
        ObjectKey {
            bus: 1,
            node,
            index,
            sub,
        }
    }

    #[test]
    fn the_dictionary_is_sorted_so_lookup_can_bisect() {
        assert!(DICTIONARY.windows(2).all(|w| w[0].index < w[1].index));
        assert_eq!(object(0x2010).map(|d| d.short), Some("valve_commanded"));
        assert_eq!(object(0x301A).map(|d| d.short), Some("valve_max_promille"));
        assert!(object(0x2017).is_none());
    }

    /// `0x3040` has one entry per TPDO kind; if the kind table grows and this
    /// doesn't, the last kind's period has no name.
    #[test]
    fn the_tpdo_period_array_covers_every_kind() {
        let def = object(0x3040).unwrap();
        assert_eq!(def.array.map(|(len, _)| len as usize), Some(iocan::KINDS.len()));
        assert_eq!(def.sub_label(1).as_deref(), Some("ValveCommanded"));
        assert_eq!(def.sub_label(18).as_deref(), Some("ValveCurrent"));
    }

    /// Real acknowledgements from the example log: the master writing valve
    /// 0's commanded position on node 5, and HCO3 on node 7.
    #[test]
    fn a_write_acknowledgement_names_the_object_it_acknowledges() {
        let ack = decode_sdo(&frame(0.0, 0x585, [0x60, 0x10, 0x20, 0x01, 0, 0, 0, 0])).unwrap();
        assert_eq!((ack.node, ack.request, ack.kind), (5, false, SdoKind::WriteAck));
        assert_eq!((ack.index, ack.sub), (0x2010, 1));
        assert_eq!(ack.value(), None);
        assert_eq!(describe_object(0x2010, 1), "0x2010.1 valve commanded state [valve 0]");
        assert_eq!(describe_object(0x2020, 3), "0x2020.3 high current output, digital level [HCO3]");
    }

    #[test]
    fn expedited_transfers_carry_values_of_the_declared_size() {
        // Write 500 ‰ to 0x2010.2 (2 bytes: n = 2 unused), read back 16.45 °C
        // counts from 0x2004.1 (i16).
        let write = decode_sdo(&frame(0.0, 0x605, [0x2B, 0x10, 0x20, 0x02, 0xF4, 0x01, 0, 0])).unwrap();
        assert_eq!((write.kind, write.size, write.value()), (SdoKind::WriteRequest, Some(2), Some(500)));
        let read = decode_sdo(&frame(0.0, 0x585, [0x4B, 0x04, 0x20, 0x01, 0x00, 0x80, 0, 0])).unwrap();
        assert_eq!((read.kind, read.value()), (SdoKind::ReadResponse, Some(-32768)));
        let request = decode_sdo(&frame(0.0, 0x605, [0x40, 0x04, 0x20, 0x01, 0, 0, 0, 0])).unwrap();
        assert_eq!(request.kind, SdoKind::ReadRequest);
    }

    #[test]
    fn an_abort_says_why() {
        let abort = decode_sdo(&frame(0.0, 0x585, [0x80, 0x20, 0x20, 0x01, 0x21, 0x00, 0x00, 0x08])).unwrap();
        assert_eq!(abort.kind, SdoKind::Abort(0x0800_0021));
        assert!(abort.detail().contains("owned by a valve"), "{}", abort.detail());
    }

    #[test]
    fn identifiers_next_to_the_sdo_range_are_not_sdo() {
        assert!(decode_sdo(&frame(0.0, 0x590, [0x60; 8])).is_none());
        assert!(decode_sdo(&frame(0.0, 0x703, [0x05; 8])).is_none());
        assert!(decode_sdo(&frame(0.0, 0x245, [0x60; 8])).is_none());
    }

    #[test]
    fn a_write_lands_when_it_is_acknowledged() {
        let log = CanOpenLog::build(&[
            frame(1.0, 0x605, [0x2B, 0x10, 0x20, 0x01, 0xE8, 0x03, 0, 0]),
            frame(1.002, 0x585, [0x60, 0x10, 0x20, 0x01, 0, 0, 0, 0]),
        ]);
        let track = &log.objects[&key(5, 0x2010, 1)];
        assert_eq!(track.writes, 1);
        let v = log.value_at(key(5, 0x2010, 1), 2.0).unwrap();
        assert_eq!((v.raw, v.origin, v.t_utc), (1000, Origin::SdoWrite, 1.002));
        assert!(log.value_at(key(5, 0x2010, 1), 1.001).is_none());
        let latency = log.transfers[1].latency.unwrap();
        assert!((latency - 0.002).abs() < 1e-9);
    }

    #[test]
    fn a_rejected_write_does_not_land() {
        let log = CanOpenLog::build(&[
            frame(1.0, 0x605, [0x2F, 0x20, 0x20, 0x01, 0x01, 0, 0, 0]),
            frame(1.001, 0x585, [0x80, 0x20, 0x20, 0x01, 0x21, 0x00, 0x00, 0x08]),
        ]);
        let track = &log.objects[&key(5, 0x2020, 1)];
        assert_eq!((track.aborts, track.last_abort), (1, Some(0x0800_0021)));
        assert!(track.changes.is_empty());
        assert_eq!(log.nodes[0].aborts, 1);
    }

    /// Without requests, the values come from the TPDOs that mirror the
    /// objects -- which is all the example log has.
    #[test]
    fn tpdos_fill_in_the_dictionary_and_only_changes_are_kept() {
        let mut position = [0u8; 8];
        position[..2].copy_from_slice(&(500u16 | 0x8000).to_le_bytes());
        let log = CanOpenLog::build(&[
            frame(1.0, 0x225, position),
            frame(1.5, 0x225, position),
            frame(2.0, 0x225, [0; 8]),
            frame(2.0, 0x305, [0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00]),
            frame(2.0, 0x245, [0x00, 0x80, 0x98, 0x08, 0, 0, 0, 0]),
        ]);
        let track = &log.objects[&key(5, 0x2012, 1)];
        assert_eq!((track.updates, track.changes.len()), (3, 2));
        let def = object(0x2012).unwrap();
        assert_eq!(def.format_value(log.value_at(key(5, 0x2012, 1), 1.7).unwrap().raw), "500 ‰, released");
        assert_eq!(log.value_at(key(5, 0x2032, 0), 3.0).map(|v| v.raw), Some(1));
        assert_eq!(log.value_at(key(5, 0x2020, 1), 3.0).map(|v| v.raw), Some(1));
        assert_eq!(log.value_at(key(5, 0x2021, 2), 3.0).map(|v| v.raw), Some(2200));
        assert!(!log.has_requests());
    }

    #[test]
    fn heartbeats_give_the_period_and_the_longest_dropout() {
        let mut frames: Vec<CanFrame> = [0.0, 1.0, 2.0, 6.0, 7.0]
            .into_iter()
            .map(|t| frame(t, 0x70B, [0x05, 0, 0, 0, 0, 0, 0, 0]))
            .collect();
        for f in &mut frames {
            f.len = 1;
        }
        // A status TPDO well before the first heartbeat must not stretch the
        // period.
        frames.insert(0, frame(-100.0, 0x30B, [0; 8]));
        let log = CanOpenLog::build(&frames);
        let node = log.node(NodeKey { bus: 1, node: 11 }).unwrap();
        assert_eq!((node.heartbeats, node.nmt_state), (5, Some(5)));
        assert_eq!(node.longest_heartbeat_gap, Some((4.0, 6.0)));
        assert_eq!(node.heartbeat_period(), Some(1.75));
    }

    #[test]
    fn an_object_plots_as_steps_with_the_flag_masked_off() {
        let mut released = [0u8; 8];
        released[..2].copy_from_slice(&(1000u16 | 0x8000).to_le_bytes());
        let log = CanOpenLog::build(&[frame(1.0, 0x225, [0; 8]), frame(3.0, 0x225, released), frame(4.0, 0x225, released)]);
        let series = log.series(key(5, 0x2012, 1)).unwrap();
        assert_eq!(series.name, "CAN_OD[5].valve_measured_0");
        assert_eq!(series.unit.as_deref(), Some("‰"));
        // Held at 0 right up to the change, then 1000 -- not a ramp between.
        assert_eq!(series.value_at(2.9, 0.0), Some(0.0));
        assert_eq!(series.value_at(3.5, 0.0), Some(1000.0));
        assert_eq!(series.value_at(4.0, 0.0), Some(1000.0));
    }

    #[test]
    fn values_format_the_way_the_firmware_documents_them() {
        assert_eq!(object(0x2032).unwrap().format_value(2), "2 fallback A");
        assert_eq!(object(0x2013).unwrap().format_value(9), "9 (unknown code)");
        assert_eq!(object(0x2002).unwrap().format_value(0b101), "bits 0, 2");
        assert_eq!(object(0x3051).unwrap().format_value(0xFF), "none");
        assert_eq!(object(0x1010).unwrap().format_value(SIGNATURE_SAVE as i64), "\"save\"");
        assert_eq!(object(0x2041).unwrap().format_value(10194), "10194 mV");
        assert_eq!(object(0x2004).unwrap().format_value(-32768), "no reading");
        assert_eq!(object(0x2000).unwrap().format_value(0xFFFF), "no reading");
        assert_eq!(object(0x2000).unwrap().format_value(707), "707 counts");
    }

    /// Slot 0 declared centi-°C, slot 1 left at centibar -- a real node 5
    /// pair of frames.
    #[test]
    fn a_sensor_slot_reads_in_the_unit_its_node_declared() {
        let log = CanOpenLog::build(&[
            frame(0.0, 0x2C5, [0x02, 0, 0, 0, 0, 0, 0, 0]),
            frame(0.0, 0x295, [0x7F, 0x07, 0x5D, 0x00, 0x00, 0x80, 0x00, 0x80]),
        ]);
        let at = |sub: u8, raw: i64| log.format_value_at(key(5, 0x2004, sub), raw, 1.0);
        assert_eq!(at(1, 1919), "1919 counts = 19.19 °C");
        assert_eq!(at(2, 93), "93 counts = 0.93 bar");
        assert_eq!(at(2, 7), "7 counts = 0.07 bar");
        assert_eq!(at(3, -32768), "no reading");
    }
}
