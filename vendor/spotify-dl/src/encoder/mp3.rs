use anyhow::anyhow;
use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm, Quality, VbrMode};

use super::{EncodedStream, Encoder, Samples};

pub struct Mp3Encoder320;
pub struct Mp3EncoderV0;

enum Mp3Profile {
    Cbr320,
    V0,
}

fn build_encoder(
    profile: Mp3Profile,
    sample_rate: u32,
    channels: u32,
) -> anyhow::Result<mp3lame_encoder::Encoder> {
    let mut builder = Builder::new().ok_or(anyhow!("Failed to create mp3 encoder"))?;

    builder
        .set_sample_rate(sample_rate)
        .map_err(|e| anyhow!("Failed to set sample rate for mp3 encoder: {}", e))?;
    builder
        .set_num_channels(channels as u8)
        .map_err(|e| anyhow!("Failed to set number of channels for mp3 encoder: {}", e))?;
    builder
        .set_quality(Quality::Best)
        .map_err(|e| anyhow!("Failed to set quality for mp3 encoder: {}", e))?;

    match profile {
        Mp3Profile::Cbr320 => {
            builder
                .set_brate(mp3lame_encoder::Bitrate::Kbps320)
                .map_err(|e| anyhow!("Failed to set bitrate for mp3 encoder: {}", e))?;
        }
        Mp3Profile::V0 => {
            builder
                .set_vbr_mode(VbrMode::Mtrh)
                .map_err(|e| anyhow!("Failed to enable VBR mode for mp3 encoder: {}", e))?;
            builder
                .set_vbr_quality(Quality::Best)
                .map_err(|e| anyhow!("Failed to set VBR quality for mp3 encoder: {}", e))?;
            builder
                .set_to_write_vbr_tag(true)
                .map_err(|e| anyhow!("Failed to enable VBR tag writing: {}", e))?;
        }
    }

    builder
        .build()
        .map_err(|e| anyhow!("Failed to build mp3 encoder: {}", e))
}

async fn encode_mp3(profile: Mp3Profile, samples: Samples) -> anyhow::Result<EncodedStream> {
    let mut mp3_encoder = build_encoder(profile, samples.sample_rate, samples.channels)?;

    let mp3_out_buffer = tokio::task::spawn_blocking(move || {
        let input = InterleavedPcm(samples.samples.as_slice());
        let mut mp3_out_buffer = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(
            samples.samples.len(),
        ));
        let encoded_size = mp3_encoder
            .encode(input, mp3_out_buffer.spare_capacity_mut())
            .map_err(|e| anyhow!("Failed to encode mp3: {}", e))?;
        unsafe {
            mp3_out_buffer.set_len(mp3_out_buffer.len().wrapping_add(encoded_size));
        }

        let encoded_size = mp3_encoder
            .flush::<FlushNoGap>(mp3_out_buffer.spare_capacity_mut())
            .map_err(|e| anyhow!("Failed to flush mp3 encoder: {}", e))?;
        unsafe {
            mp3_out_buffer.set_len(mp3_out_buffer.len().wrapping_add(encoded_size));
        }
        Ok::<Vec<u8>, anyhow::Error>(mp3_out_buffer)
    })
    .await??;

    Ok(EncodedStream::new(mp3_out_buffer))
}

#[async_trait::async_trait]
impl Encoder for Mp3Encoder320 {
    async fn encode(&self, samples: Samples) -> anyhow::Result<EncodedStream> {
        encode_mp3(Mp3Profile::Cbr320, samples).await
    }
}

#[async_trait::async_trait]
impl Encoder for Mp3EncoderV0 {
    async fn encode(&self, samples: Samples) -> anyhow::Result<EncodedStream> {
        encode_mp3(Mp3Profile::V0, samples).await
    }
}
