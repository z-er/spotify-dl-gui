use anyhow::anyhow;

use super::{EncodedStream, Encoder, Samples};

pub struct WavEncoder;

#[async_trait::async_trait]
impl Encoder for WavEncoder {
    async fn encode(&self, samples: Samples) -> anyhow::Result<EncodedStream> {
        tokio::task::spawn_blocking(move || encode_wav(samples)).await?
    }
}

fn encode_wav(samples: Samples) -> anyhow::Result<EncodedStream> {
    let bits_per_sample = if samples.bits_per_sample >= 24 {
        24
    } else {
        16
    };
    let sample_format = hound::SampleFormat::Int;
    let spec = hound::WavSpec {
        channels: samples.channels as u16,
        sample_rate: samples.sample_rate,
        bits_per_sample: bits_per_sample as u16,
        sample_format,
    };

    let mut output = Vec::new();
    let cursor = std::io::Cursor::new(&mut output);
    let mut writer = hound::WavWriter::new(cursor, spec)
        .map_err(|e| anyhow!("Failed to create wav writer: {e}"))?;

    if bits_per_sample == 24 {
        for sample in samples.to_s24() {
            writer
                .write_sample(sample)
                .map_err(|e| anyhow!("Failed to write wav sample: {e}"))?;
        }
    } else {
        for sample in samples.samples {
            let reduced = (sample >> 16) as i16;
            writer
                .write_sample(reduced)
                .map_err(|e| anyhow!("Failed to write wav sample: {e}"))?;
        }
    }

    writer
        .finalize()
        .map_err(|e| anyhow!("Failed to finalize wav file: {e}"))?;

    Ok(EncodedStream::new(output))
}
