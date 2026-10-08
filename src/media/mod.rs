//! Native local preview/codec work, separate from Discord transport.
pub mod datagram;
#[cfg(target_os = "macos")]
pub mod macos_capture;
#[cfg(target_os = "macos")]
pub mod macos_codec;
pub mod rtp;
