//! Command-line interface.
//!
//! DESIGN §12: parse at the boundary. Every knob is turned into a typed value
//! here and passed down as `PlayerConfig`, so no module below reads the
//! environment or handles a raw string.

use std::path::PathBuf;

use clap::Parser;

use crate::audio::{PlayerConfig, resample::Quality};

/// gmuse — Gabriel's MUSical Experience.
///
/// A cmus-shaped terminal music player. Until the UI lands, this drives the
/// player from stdin lines; run without a file and type `help`.
#[derive(Debug, Parser)]
#[command(name = "gmuse", version, about, long_about = None)]
pub struct Cli {
    /// Audio file to load and play immediately.
    pub file: Option<PathBuf>,

    /// Output device, matched by substring against the device id.
    #[arg(short, long, env = "GMUSE_DEVICE")]
    pub device: Option<String>,

    /// Force the device sample rate, in Hz. Mainly for measuring the cost of
    /// sample-rate conversion; normally the device picks.
    #[arg(short, long, env = "GMUSE_RATE")]
    pub rate: Option<u32>,

    /// Resampler quality.
    #[arg(short = 'q', long, env = "GMUSE_RESAMPLE", default_value = "balanced")]
    pub resample: Quality,

    /// Initial volume, 0.0 to 2.0.
    #[arg(short = 'v', long, default_value_t = 1.0)]
    pub volume: f32,
}

impl Cli {
    pub fn player_config(&self) -> PlayerConfig {
        PlayerConfig {
            device: self.device.clone(),
            rate: self.rate,
            resample: self.resample,
            volume: self.volume,
        }
    }
}
