//! Generates Rust bindings for the `rapid` MAVLink dialect (the common
//! dialect plus our project-specific messages, e.g. `PRESSURE_VESSEL`) from
//! the XML definitions in `mavlink_dialects/`, via `mavlink-bindgen` -- the
//! same code generator the `mavlink` crate itself uses for its bundled
//! dialects. We generate our own because a custom dialect can't be selected
//! through that crate's `dialect-*` Cargo features.

use std::env;
use std::path::Path;

use mavlink_bindgen::XmlDefinitions;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let dialects_dir = Path::new(&manifest_dir).join("mavlink_dialects");
    let out_dir = env::var("OUT_DIR").unwrap();

    let result = mavlink_bindgen::generate(XmlDefinitions::Directory(dialects_dir), &out_dir)
        .expect("failed to generate MAVLink dialect bindings");

    mavlink_bindgen::format_generated_code(&result);
    mavlink_bindgen::emit_cargo_build_messages(&result);
}
