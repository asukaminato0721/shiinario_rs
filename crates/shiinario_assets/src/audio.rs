//! Shiina Rio OGV is an Ogg/Vorbis stream in a RIFF-like wrapper.
use anyhow::{Context, Result, ensure};
pub fn ogg_stream(data: &[u8]) -> Result<&[u8]> {
    if data.starts_with(b"OggS") {
        return Ok(data);
    }
    ensure!(data.starts_with(b"OGV\0"), "unsupported audio wrapper");
    ensure!(
        data.get(12..16) == Some(b"fmt "),
        "missing OGV format chunk"
    );
    let n = u32::from_le_bytes(
        data.get(16..20)
            .context("truncated OGV header")?
            .try_into()?,
    ) as usize;
    let p = 20usize.checked_add(n).context("OGV offset overflow")?;
    ensure!(
        data.get(p..p + 4) == Some(b"data"),
        "missing OGV data chunk"
    );
    // The data length describes decoded PCM, not the Ogg payload length.
    // GARbro likewise uses the remainder of the file as the bounded stream.
    let stream = data.get(p + 8..).context("truncated OGV stream")?;
    ensure!(stream.starts_with(b"OggS"), "OGV payload is not Ogg");
    Ok(stream)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bad_wrapper() {
        assert!(ogg_stream(b"OGV\0").is_err());
        assert!(ogg_stream(b"RIFF").is_err());
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioInfo {
    pub channels: u8,
    pub sample_rate: u32,
    pub samples_per_channel: u64,
}
/// Decode incrementally: callers can stream PCM into their host audio queue.
/// The cap also bounds work for malicious streams with excessive packet counts.
pub fn decode(data: &[u8], mut output: impl FnMut(&[i16]) -> Result<()>) -> Result<AudioInfo> {
    let stream = ogg_stream(data)?;
    let wrapper = if data.starts_with(b"OGV\0") {
        let format_size = u32::from_le_bytes(data[16..20].try_into()?) as usize;
        ensure!(format_size >= 16, "truncated OGV PCM format");
        let format = data.get(20..36).context("truncated OGV PCM format")?;
        let word = |p| u16::from_le_bytes([format[p], format[p + 1]]);
        let rate = u32::from_le_bytes(format[4..8].try_into()?);
        let byte_rate = u32::from_le_bytes(format[8..12].try_into()?);
        let channels = word(2);
        ensure!(
            word(0) == 1 && word(14) == 16 && channels > 0 && channels <= 8,
            "unsupported OGV PCM format"
        );
        ensure!(
            word(12) == channels * 2
                && u64::from(byte_rate) == u64::from(rate) * u64::from(channels) * 2,
            "inconsistent OGV PCM format"
        );
        let length_pos = 24 + format_size;
        let length = u32::from_le_bytes(
            data.get(length_pos..length_pos + 4)
                .context("truncated OGV PCM length")?
                .try_into()?,
        );
        ensure!(
            length % (u32::from(channels) * 2) == 0,
            "unaligned OGV PCM length"
        );
        ensure!(length <= 512 * 1024 * 1024, "OGV PCM length exceeds cap");
        Some((channels as u8, rate, u64::from(length / 2)))
    } else {
        None
    };
    // wav2ogv appends eight zero bytes after the end-of-stream page. Bound the
    // Vorbis reader at EOS, preserving strict CRC checks on actual Ogg pages.
    let mut end = 0usize;
    loop {
        let header = stream.get(end..end + 27).context("truncated Ogg page")?;
        ensure!(
            &header[..4] == b"OggS" && header[4] == 0,
            "invalid Ogg page"
        );
        let count = header[26] as usize;
        let lacing = stream
            .get(end + 27..end + 27 + count)
            .context("truncated Ogg lacing")?;
        let size: usize = lacing.iter().map(|&n| n as usize).sum();
        end = end
            .checked_add(27 + count + size)
            .context("Ogg page size overflow")?;
        ensure!(end <= stream.len(), "truncated Ogg payload");
        if header[5] & 4 != 0 {
            break;
        }
    }
    let tail = &stream[end..];
    ensure!(
        tail.is_empty() || (tail.len() <= 8 && tail.iter().all(|&b| b == 0)),
        "unexpected bytes after Ogg EOS"
    );
    let stream = &stream[..end];
    let mut decoder = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(stream))
        .map_err(|e| anyhow::anyhow!("Vorbis header: {e:?}"))?;
    let channels = decoder.ident_hdr.audio_channels;
    let sample_rate = decoder.ident_hdr.audio_sample_rate;
    if let Some((expected_channels, expected_rate, _)) = wrapper {
        ensure!(
            channels == expected_channels && sample_rate == expected_rate,
            "OGV format differs from Vorbis stream"
        );
    }
    ensure!(
        channels > 0 && channels <= 8 && sample_rate > 0 && sample_rate <= 384000,
        "invalid Vorbis stream parameters"
    );
    let mut samples = 0u64;
    let mut emitted = 0u64;
    while let Some(packet) = decoder
        .read_dec_packet_itl()
        .map_err(|e| anyhow::anyhow!("Vorbis packet: {e:?}"))?
    {
        ensure!(
            packet.len() % channels as usize == 0,
            "incomplete interleaved audio frame"
        );
        samples += packet.len() as u64;
        ensure!(samples <= 256 * 1024 * 1024, "audio sample limit exceeded");
        // The original buffer size comes from the OGV PCM length. Vorbis can
        // produce padding beyond that size, which must not extend playback.
        let count = wrapper.map_or(packet.len(), |(_, _, limit)| {
            packet.len().min(limit.saturating_sub(emitted) as usize)
        });
        if count > 0 {
            output(&packet[..count])?;
            emitted += count as u64;
        }
    }
    if let Some((_, _, limit)) = wrapper {
        ensure!(
            emitted == limit,
            "Vorbis stream is shorter than declared OGV PCM length"
        );
    }
    Ok(AudioInfo {
        channels,
        sample_rate,
        samples_per_channel: emitted / channels as u64,
    })
}
