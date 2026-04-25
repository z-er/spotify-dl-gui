mod alac;
mod flac;
#[cfg(feature = "mp3")]
mod mp3;
pub mod tags;
mod wav;

use std::{path::Path, str::FromStr};

use anyhow::Result;

use self::{
    alac::AlacCafEncoder,
    flac::FlacEncoder,
    mp3::{Mp3Encoder320, Mp3EncoderV0},
    wav::WavEncoder,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Format {
    Alac,
    Flac,
    #[cfg(feature = "mp3")]
    Mp3,
    #[cfg(feature = "mp3")]
    Mp3V0,
    Wav,
}

impl FromStr for Format {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "alac" => Ok(Format::Alac),
            "flac" => Ok(Format::Flac),
            #[cfg(feature = "mp3")]
            "mp3" => Ok(Format::Mp3),
            #[cfg(feature = "mp3")]
            "mp3-v0" => Ok(Format::Mp3V0),
            "wav" => Ok(Format::Wav),
            _ => Err(anyhow::anyhow!("Unsupported format")),
        }
    }
}

impl Format {
    pub fn extension(&self) -> &'static str {
        match self {
            Format::Alac => "caf",
            Format::Flac => "flac",
            #[cfg(feature = "mp3")]
            Format::Mp3 => "mp3",
            #[cfg(feature = "mp3")]
            Format::Mp3V0 => "mp3",
            Format::Wav => "wav",
        }
    }
}

const ALAC_ENCODER: &AlacCafEncoder = &AlacCafEncoder;
const FLAC_ENCODER: &FlacEncoder = &FlacEncoder;
#[cfg(feature = "mp3")]
const MP3_320_ENCODER: &Mp3Encoder320 = &Mp3Encoder320;
#[cfg(feature = "mp3")]
const MP3_V0_ENCODER: &Mp3EncoderV0 = &Mp3EncoderV0;
const WAV_ENCODER: &WavEncoder = &WavEncoder;

pub fn get_encoder(format: Format) -> &'static dyn Encoder {
    match format {
        Format::Alac => ALAC_ENCODER,
        Format::Flac => FLAC_ENCODER,
        #[cfg(feature = "mp3")]
        Format::Mp3 => MP3_320_ENCODER,
        #[cfg(feature = "mp3")]
        Format::Mp3V0 => MP3_V0_ENCODER,
        Format::Wav => WAV_ENCODER,
    }
}

#[async_trait::async_trait]
pub trait Encoder {
    async fn encode(&self, samples: Samples) -> Result<EncodedStream>;
}

pub struct Samples {
    pub samples: Vec<i32>,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
}

impl Samples {
    pub fn new(samples: Vec<i32>, sample_rate: u32, channels: u32, bits_per_sample: u32) -> Self {
        Samples {
            samples,
            sample_rate,
            channels,
            bits_per_sample,
        }
    }

    pub fn to_s24(&self) -> Vec<i32> {
        self.samples
            .iter()
            .map(|&sample| (sample >> 8) as i32) // Convert to S24 by shifting down
            .collect()
    }
}

impl Default for Samples {
    fn default() -> Self {
        Samples {
            samples: Vec::new(),
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 32,
        }
    }
}

pub struct EncodedStream {
    pub stream: Vec<u8>,
}

impl EncodedStream {
    pub fn new(stream: Vec<u8>) -> Self {
        EncodedStream { stream }
    }

    pub async fn write_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        if !path.as_ref().exists() {
            tokio::fs::create_dir_all(
                path.as_ref()
                    .parent()
                    .ok_or(anyhow::anyhow!("Could not create path"))?,
            )
            .await?;
        }
        tokio::fs::write(path, &self.stream).await?;
        Ok(())
    }
}
