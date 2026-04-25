use alac_encoder::{AlacEncoder, DEFAULT_FRAME_SIZE, FormatDescription};
use anyhow::anyhow;

use super::{EncodedStream, Encoder, Samples};

const ALAC_FORMAT_FLAG_16_BIT_SOURCE_DATA: u32 = 1;

pub struct AlacCafEncoder;

#[async_trait::async_trait]
impl Encoder for AlacCafEncoder {
    async fn encode(&self, samples: Samples) -> anyhow::Result<EncodedStream> {
        tokio::task::spawn_blocking(move || encode_alac_caf(samples)).await?
    }
}

fn encode_alac_caf(samples: Samples) -> anyhow::Result<EncodedStream> {
    let channels = samples.channels.max(1);
    let input_format = FormatDescription::pcm::<i16>(samples.sample_rate as f64, channels);
    let output_format = FormatDescription::alac(
        samples.sample_rate as f64,
        DEFAULT_FRAME_SIZE as u32,
        channels,
    );
    let mut encoder = AlacEncoder::new(&output_format);
    let frame_bytes = DEFAULT_FRAME_SIZE * channels as usize * 2;
    let pcm_bytes = pcm16_bytes(&samples.samples);
    let mut packet_buffer = vec![0u8; output_format.max_packet_size()];
    let mut packets = Vec::new();
    let mut valid_frames = 0u64;

    for chunk in pcm_bytes.chunks(frame_bytes) {
        let size = encoder.encode(&input_format, chunk, &mut packet_buffer);
        packets.push(packet_buffer[..size].to_vec());
        valid_frames += (chunk.len() / (channels as usize * 2)) as u64;
    }

    let remainder_frames = if valid_frames == 0 {
        0
    } else {
        let remainder = (valid_frames as u32) % (DEFAULT_FRAME_SIZE as u32);
        if remainder == 0 {
            0
        } else {
            DEFAULT_FRAME_SIZE as u32 - remainder
        }
    };

    let audio_description = build_audio_description(samples.sample_rate, channels);
    let magic_cookie = encoder.magic_cookie();
    let packet_table = build_packet_table(&packets, valid_frames, remainder_frames);
    let data_chunk = build_data_chunk(&packets);

    let mut caf = Vec::new();
    caf.extend_from_slice(b"caff");
    caf.extend_from_slice(&1u16.to_be_bytes());
    caf.extend_from_slice(&0u16.to_be_bytes());
    write_chunk(&mut caf, b"desc", &audio_description)?;
    write_chunk(&mut caf, b"kuki", &magic_cookie)?;
    write_chunk(&mut caf, b"pakt", &packet_table)?;
    write_chunk(&mut caf, b"data", &data_chunk)?;

    Ok(EncodedStream::new(caf))
}

fn pcm16_bytes(samples: &[i32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&((*sample >> 16) as i16).to_ne_bytes());
    }
    bytes
}

fn build_audio_description(sample_rate: u32, channels: u32) -> Vec<u8> {
    let mut description = Vec::with_capacity(32);
    description.extend_from_slice(&(sample_rate as f64).to_be_bytes());
    description.extend_from_slice(b"alac");
    description.extend_from_slice(&ALAC_FORMAT_FLAG_16_BIT_SOURCE_DATA.to_be_bytes());
    description.extend_from_slice(&0u32.to_be_bytes());
    description.extend_from_slice(&(DEFAULT_FRAME_SIZE as u32).to_be_bytes());
    description.extend_from_slice(&channels.to_be_bytes());
    description.extend_from_slice(&0u32.to_be_bytes());
    description
}

fn build_packet_table(packets: &[Vec<u8>], valid_frames: u64, remainder_frames: u32) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend_from_slice(&(packets.len() as i64).to_be_bytes());
    table.extend_from_slice(&(valid_frames as i64).to_be_bytes());
    table.extend_from_slice(&0i32.to_be_bytes());
    table.extend_from_slice(&(remainder_frames as i32).to_be_bytes());

    for packet in packets {
        write_caf_varint(&mut table, packet.len() as u64);
    }

    table
}

fn build_data_chunk(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_be_bytes());
    for packet in packets {
        data.extend_from_slice(packet);
    }
    data
}

fn write_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> anyhow::Result<()> {
    let chunk_size =
        i64::try_from(data.len()).map_err(|_| anyhow!("CAF chunk too large to serialize"))?;
    output.extend_from_slice(kind);
    output.extend_from_slice(&chunk_size.to_be_bytes());
    output.extend_from_slice(data);
    Ok(())
}

fn write_caf_varint(output: &mut Vec<u8>, value: u64) {
    let mut stack = [0u8; 10];
    let mut index = stack.len();
    let mut remaining = value;

    stack[index - 1] = (remaining & 0x7f) as u8;
    index -= 1;
    remaining >>= 7;

    while remaining > 0 {
        stack[index - 1] = ((remaining & 0x7f) as u8) | 0x80;
        index -= 1;
        remaining >>= 7;
    }

    output.extend_from_slice(&stack[index..]);
}
