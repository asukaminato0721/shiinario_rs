//! From GARbro ArcFormats/ShiinaRio/AudioPAD.cs. Copyright (C) 2015 morkt (MIT).
use super::AudioInfo;
use anyhow::{Context, Result, ensure};

pub(super) fn decode(
    data: &[u8],
    mut output: impl FnMut(&[i16]) -> Result<()>,
) -> Result<AudioInfo> {
    let header = data.get(..44).context("truncated PAD header")?;
    ensure!(
        &header[8..16] == b"WAVEfmt " && &header[36..40] == b"data",
        "invalid PAD header"
    );
    let word = |p| u16::from_le_bytes([header[p], header[p + 1]]);
    let channels = word(22) as usize;
    let rate = u32::from_le_bytes(header[24..28].try_into()?);
    let length = u32::from_le_bytes(header[40..44].try_into()?) as usize;
    ensure!(
        word(20) == 1 && word(34) == 16 && (1..=2).contains(&channels) && rate > 0,
        "unsupported PAD PCM format"
    );
    ensure!(
        length <= 512 * 1024 * 1024 && length.is_multiple_of(channels * 2),
        "invalid PAD PCM length"
    );
    let coefficients = [
        [0.0, 0.0],
        [0.9375, 0.0],
        [1.796875, -0.8125],
        [1.53125, -0.859375],
        [1.90625, -0.9375],
    ];
    let mut history = [[0.0f64; 2]; 2];
    let mut pos = 44;
    let mut written = 0;
    while *data.get(pos).context("PAD has no end marker")? != 255 {
        let size = channels * 16;
        let block = data.get(pos..pos + size).context("truncated PAD block")?;
        ensure!(written < length, "PAD has excess blocks");
        let mut pcm = Vec::with_capacity(channels * 28);
        for i in 0..28 {
            for channel in 0..channels {
                let param = block[channel * 2 + 1];
                let coef = coefficients
                    .get((param >> 4) as usize)
                    .context("invalid PAD predictor")?;
                let packed = block[channels * 2 + channel * 14 + i / 2];
                let nibble = if i & 1 == 0 { packed & 15 } else { packed >> 4 };
                let residual = ((i16::from(nibble) << 12) >> (param & 15)) as f64;
                let [previous, older] = history[channel];
                let value = residual + (older * coef[1] + previous * coef[0]);
                history[channel] = [value, previous];
                // GARbro rounds with +0.5 and casts without saturation.
                pcm.push((value + 0.5) as i32 as i16);
            }
        }
        let count = pcm.len().min((length - written) / 2);
        output(&pcm[..count])?;
        written += count * 2;
        pos += size;
    }
    ensure!(
        written == length,
        "PAD stream ends before declared PCM length"
    );
    Ok(AudioInfo {
        channels: channels as u8,
        sample_rate: rate,
        samples_per_channel: (length / (channels * 2)) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pad(channels: u16) -> Vec<u8> {
        let mut data = b"PAD\0\0\0\0\0WAVEfmt ".to_vec();
        data.extend(16u32.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(channels.to_le_bytes());
        data.extend(22050u32.to_le_bytes());
        data.extend((44100u32 * u32::from(channels)).to_le_bytes());
        data.extend((channels * 2).to_le_bytes());
        data.extend(16u16.to_le_bytes());
        data.extend(b"data");
        data.extend((56u32 * u32::from(channels)).to_le_bytes());
        for _ in 0..channels {
            data.extend([0, 12]);
        }
        data.extend([0x81; 14]);
        if channels == 2 {
            data.extend([0x27; 14]);
        }
        data.push(255);
        data
    }
    #[test]
    fn original_garbro_predictors_and_history() {
        for (data, expected) in [
            (
                &include_bytes!("../tests/fixtures/garbro/pad-1.pad")[..],
                &include_bytes!("../tests/fixtures/garbro/pad-1.pcm")[..],
            ),
            (
                &include_bytes!("../tests/fixtures/garbro/pad-2.pad")[..],
                &include_bytes!("../tests/fixtures/garbro/pad-2.pcm")[..],
            ),
        ] {
            let mut pcm = vec![];
            decode(data, |samples| {
                for sample in samples {
                    pcm.extend(sample.to_le_bytes());
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(pcm, expected);
            let wav = super::super::wav_stream(data).unwrap();
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[44..], expected);
        }
    }
    #[test]
    fn mono_and_stereo_nibbles() {
        for channels in [1, 2] {
            let mut pcm = vec![];
            let info = decode(&pad(channels), |samples| {
                pcm.extend_from_slice(samples);
                Ok(())
            })
            .unwrap();
            assert_eq!(info.samples_per_channel, 28);
            assert_eq!(
                pcm,
                if channels == 1 {
                    [1, -7].repeat(14)
                } else {
                    [1, 7, -7, 2].repeat(14)
                }
            );
        }
    }
    #[test]
    fn invalid_predictor_and_truncation() {
        let mut data = pad(1);
        data[45] = 0xf0;
        assert!(decode(&data, |_| Ok(())).is_err());
        let data = pad(2);
        for len in 0..data.len() {
            assert!(decode(&data[..len], |_| Ok(())).is_err());
        }
    }
}
