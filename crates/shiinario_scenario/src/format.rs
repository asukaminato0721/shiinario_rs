//! Bounded subset of ANSI wsprintf used by the verified SCN instructions.
use anyhow::{Context, Result, bail, ensure};

pub(crate) fn ansi_format(
    format: &[u8],
    arguments: &[u32],
    string_at: impl FnMut(u32) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    ansi_format_with(
        format,
        |index| {
            arguments
                .get(index)
                .copied()
                .context("missing format argument")
        },
        string_at,
    )
}

pub(crate) fn ansi_format_with(
    format: &[u8],
    mut argument_at: impl FnMut(usize) -> Result<u32>,
    mut string_at: impl FnMut(u32) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut pos = 0;
    let mut argument = 0;
    while pos < format.len() {
        let byte = format[pos];
        pos += 1;
        if byte != b'%' {
            output.push(byte);
        } else if format.get(pos) == Some(&b'%') {
            output.push(b'%');
            pos += 1;
        } else {
            let zero = format.get(pos) == Some(&b'0');
            let mut width = 0usize;
            while let Some(&digit @ b'0'..=b'9') = format.get(pos) {
                width = width * 10 + usize::from(digit - b'0');
                ensure!(width <= 1023, "formatted field exceeds 1023 bytes");
                pos += 1;
            }
            let conversion = *format.get(pos).context("truncated format conversion")?;
            pos += 1;
            ensure!(argument < 256, "format exceeds 256 arguments");
            let value = argument_at(argument)?;
            argument += 1;
            let text = match conversion {
                b'd' | b'i' => (value as i32).to_string().into_bytes(),
                b'u' => value.to_string().into_bytes(),
                b'x' => format!("{value:x}").into_bytes(),
                b'X' => format!("{value:X}").into_bytes(),
                b's' => string_at(value)?,
                b'c' => vec![value as u8],
                _ => bail!("unresolved wsprintf conversion {conversion:#x}"),
            };
            let padding = width.saturating_sub(text.len());
            let zero = zero && !matches!(conversion, b's' | b'c');
            if zero && text.starts_with(b"-") {
                output.push(b'-');
                output.extend(std::iter::repeat_n(b'0', padding));
                output.extend_from_slice(&text[1..]);
            } else {
                output.extend(std::iter::repeat_n(if zero { b'0' } else { b' ' }, padding));
                output.extend_from_slice(&text);
            }
        }
        ensure!(output.len() <= 1023, "formatted string exceeds 1023 bytes");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_missing_arguments_unsupported_conversions_and_large_output() {
        for format in [b"%".as_slice(), b"%s", b"%999999999999d", b"%d%d"] {
            assert!(ansi_format(format, &[0], |_| bail!("invalid string address")).is_err());
        }
        assert!(ansi_format(&[b'a'; 1024], &[], |_| unreachable!()).is_err());
        assert_eq!(
            ansi_format(b"%d/%u", &[u32::MAX, 7], |_| unreachable!()).unwrap(),
            b"-1/7"
        );
    }
    #[test]
    fn string_fields_preserve_ansi_bytes_and_enforce_the_output_limit() {
        assert_eq!(
            ansi_format(b"_F%s/", &[123], |address| {
                assert_eq!(address, 123);
                Ok(b"\x82l\x82r".to_vec())
            })
            .unwrap(),
            b"_F\x82l\x82r/"
        );
        assert_eq!(
            ansi_format(b"%05s/%c", &[123, 65], |_| Ok(b"AB".to_vec())).unwrap(),
            b"   AB/A"
        );
        assert!(ansi_format(b"%s", &[123], |_| Ok(vec![b'x'; 1024])).is_err());
    }
}
