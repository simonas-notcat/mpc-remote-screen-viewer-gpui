//! Portable client core. See SPEC.md for behavior and validation requirements.
//! No network connection is made implicitly.

pub mod analyzer;
pub mod audio;
#[cfg(feature = "audio-output")]
pub mod audio_output;
pub mod auth;
pub mod cli;
pub mod control;
pub mod framebuffer;
pub mod information;
pub mod input;
#[cfg(all(feature = "mcp", unix))]
pub mod mcp;
pub mod playout;
pub mod preferences;
pub mod protocol;
pub mod recording;
pub mod session;
pub mod spectrogram;
pub mod statistics;
#[cfg(unix)]
pub mod teacher;
pub mod vectorscope;
pub mod viewport;
pub mod waveform;
