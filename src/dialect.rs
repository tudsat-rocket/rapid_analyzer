//! Our own MAVLink dialect: the common message set plus the project's
//! `Rapid` extension (`PRESSURE_VESSEL`, `VALVE`, `ROCKET_INFO`), generated
//! at build time by `build.rs` from `mavlink_dialects/`. See
//! [`crate::import::tlog`] for why we can't just use the `mavlink` crate's
//! bundled `ardupilotmega`/`common` dialects.

#[allow(
    non_camel_case_types,
    clippy::derive_partial_eq_without_eq,
    clippy::field_reassign_with_default,
    non_snake_case,
    clippy::unnecessary_cast,
    clippy::bad_bit_mask,
    clippy::suspicious_else_formatting
)]
pub mod dialects {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

pub use dialects::rapid;
