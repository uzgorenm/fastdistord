//! Media components for native camera and screen-sharing work.
//! Capture adapters are separate from the offline encrypted video pipeline.
pub mod media;
pub mod video;

/// User-facing release label; Cargo uses its corresponding SemVer.
pub const RELEASE_VERSION: &str = "0.01";
