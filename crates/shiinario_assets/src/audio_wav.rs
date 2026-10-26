//! Bounded RIFF/WAVE PCM decoding for native sound-effect files.
use super::AudioInfo;
use anyhow::{Context, Result, ensure};

pub(super) fn decode(
    data: &[u8],
    mut output: impl FnMut(&[i16]) -> Result<()>,
) -> Result<AudioInfo> {
    let header = data.get(..12).context("truncated WAV header")?;
    ensure!(
        &header[..4] == b"RIFF" && &header[8..] == b"WAVE",
        "invalid WAV header"
    );
    let length = u32::from_le_bytes(header[4..8].try_into()?) as usize;
    let end = length.checked_add(8).context("WAV length overflow")?;
    ensure!(end >= 12 && end <= data.len(), "invalid WAV RIFF length");
    let mut format = None;
    let mut pcm = None;
    let mut position = 12;
    while position < end {
        let chunk = data
            .get(position..position + 8)
            .filter(|_| position + 8 <= end)
            .context("truncated WAV chunk header")?;
        let size = u32::from_le_bytes(chunk[4..8].try_into()?) as usize;
        let start = position + 8;
        let chunk_end = start
            .checked_add(size)
            .context("WAV chunk length overflow")?;
        let bytes = data
            .get(start..chunk_end)
            .filter(|_| chunk_end <= end)
            .context("truncated WAV chunk")?;
        match &chunk[..4] {
            b"fmt " => {
                ensure!(format.is_none(), "duplicate WAV format chunk");
                format = Some(bytes);
            }
            b"data" => {
                ensure!(pcm.is_none(), "duplicate WAV data chunk");
                pcm = Some(bytes);
            }
            _ => {}
        }
        position = chunk_end
            .checked_add(size & 1)
            .context("WAV padding overflow")?;
        ensure!(position <= end, "missing WAV chunk padding");
    }
    let format = format.context("missing WAV format chunk")?;
    ensure!(format.len() >= 16, "truncated WAV PCM format");
    let word = |offset| u16::from_le_bytes([format[offset], format[offset + 1]]);
    let channels = word(2);
    let bits = word(14);
    let rate = u32::from_le_bytes(format[4..8].try_into()?);
    let byte_rate = u32::from_le_bytes(format[8..12].try_into()?);
    ensure!(
        word(0) == 1 && (1..=8).contains(&channels) && matches!(bits, 8 | 16),
        "unsupported WAV PCM format"
    );
    ensure!((1..=384000).contains(&rate), "invalid WAV sample rate");
    let alignment = channels * (bits / 8);
    ensure!(
        word(12) == alignment && u64::from(byte_rate) == u64::from(rate) * u64::from(alignment),
        "inconsistent WAV PCM format"
    );
    let pcm = pcm.context("missing WAV data chunk")?;
    ensure!(
        pcm.len() <= 512 * 1024 * 1024 && pcm.len().is_multiple_of(alignment as usize),
        "invalid WAV PCM length"
    );
    let block = 4096 * alignment as usize;
    for bytes in pcm.chunks(block) {
        let samples: Vec<i16> = if bits == 8 {
            bytes
                .iter()
                .map(|&value| (i16::from(value) - 128) << 8)
                .collect()
        } else {
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
                .collect()
        };
        output(&samples)?;
    }
    Ok(AudioInfo {
        channels: channels as u8,
        sample_rate: rate,
        samples_per_channel: (pcm.len() / alignment as usize) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(bits: u16, channels: u16, pcm: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVEJUNK\x01\0\0\0X\0fmt ".to_vec();
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(22050u32.to_le_bytes());
        let alignment = channels * (bits / 8);
        bytes.extend((22050 * u32::from(alignment)).to_le_bytes());
        bytes.extend(alignment.to_le_bytes());
        bytes.extend(bits.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend((pcm.len() as u32).to_le_bytes());
        bytes.extend(pcm);
        if pcm.len() & 1 != 0 {
            bytes.push(0);
        }
        let length = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        bytes
    }

    #[test]
    fn pcm_samples_preserve_channel_order_and_unsigned_eight_bit_silence() {
        for (bits, channels, pcm, expected) in [
            (8, 1, vec![0, 128, 255], vec![i16::MIN, 0, 32512]),
            (
                16,
                2,
                vec![0, 128, 255, 127, 0, 0, 255, 255],
                vec![i16::MIN, i16::MAX, 0, -1],
            ),
        ] {
            let bytes = fixture(bits, channels, &pcm);
            let mut samples: Vec<i16> = Vec::new();
            let info = decode(&bytes, |block| {
                samples.extend(block);
                Ok(())
            })
            .unwrap();
            assert_eq!(samples, expected);
            assert_eq!(info.channels, channels as u8);
            assert_eq!(info.sample_rate, 22050);
            assert_eq!(
                info.samples_per_channel,
                (expected.len() / channels as usize) as u64
            );
            for end in 0..bytes.len() {
                assert!(decode(&bytes[..end], |_| Ok(())).is_err());
            }
        }
    }

    #[test]
    fn malformed_formats_and_partial_frames_fail_before_output() {
        for (bits, channels, pcm) in [(24, 1, vec![0; 3]), (16, 0, vec![]), (16, 2, vec![0; 2])] {
            assert!(
                decode(&fixture(bits, channels, &pcm), |_| panic!(
                    "unexpected output"
                ))
                .is_err()
            );
        }
        let mut bytes = fixture(16, 1, &[0; 2]);
        bytes[42..44].copy_from_slice(&0u16.to_le_bytes());
        assert!(decode(&bytes, |_| panic!("unexpected output")).is_err());
        assert!(decode(&fixture(8, 1, &[128]), |_| anyhow::bail!("output failure")).is_err());
    }
}
