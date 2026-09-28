pub mod app;
#[cfg(feature = "media")]
pub mod audio_playback;
pub mod can;
pub mod can_builder;
pub mod canopen_inspector;
pub mod colors;
pub mod dialect;
pub mod export;
pub mod import;
pub mod mavlink_meta;
pub mod model;
pub mod n2o;
pub mod panes;
pub mod series;
pub mod tank;
pub mod timeline;
pub mod vapor;
#[cfg(target_arch = "wasm32")]
pub mod web;
#[cfg(feature = "media")]
pub mod video_worker;

// Without the `media` feature (the web build) nothing can create a video or
// audio source, but the panes still hold a slot per source for the decoder
// and the player. These stand in for them: types with no values, so the
// slots stay empty and the code around them compiles unchanged.
#[cfg(not(feature = "media"))]
pub mod audio_playback {
    pub enum AudioPlayback {}
}
#[cfg(not(feature = "media"))]
pub mod video_worker {
    pub enum VideoWorker {}
}
