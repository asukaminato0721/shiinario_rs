//! Bounded subset of ANSI wsprintf used by the verified SCN instructions.
use anyhow::{Context, Result, bail, ensure};

pub(crate) fn integer_format(format: &[u8], arguments: &[u32]) -> Result<Vec<u8>> {
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
            let value = *arguments.get(argument).context("missing format argument")?;
            argument += 1;
            let text = match conversion {
                b'd' => (value as i32).to_string(),
                b'u' => value.to_string(),
                b'x' => format!("{value:x}"),
                b'X' => format!("{value:X}"),
                _ => bail!("unresolved wsprintf conversion {conversion:#x}"),
            };
            let padding = width.saturating_sub(text.len());
            if zero && text.starts_with('-') {
                output.push(b'-');
                output.extend(std::iter::repeat_n(b'0', padding));
                output.extend_from_slice(&text.as_bytes()[1..]);
            } else {
                output.extend(std::iter::repeat_n(if zero { b'0' } else { b' ' }, padding));
                output.extend_from_slice(text.as_bytes());
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
    fn matches_wine_wsprintf_integer_cases() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/wsprintf-probe.json"))
                .unwrap();
        for case in cases["cases"].as_array().unwrap() {
            assert_eq!(
                integer_format(
                    case["format"].as_str().unwrap().as_bytes(),
                    &[case["value"].as_u64().unwrap() as u32]
                )
                .unwrap(),
                case["output"].as_str().unwrap().as_bytes()
            );
        }
    }
    #[test]
    fn rejects_missing_arguments_unsupported_conversions_and_large_output() {
        for format in [b"%".as_slice(), b"%s", b"%999999999999d", b"%d%d"] {
            assert!(integer_format(format, &[0]).is_err());
        }
        assert!(integer_format(&[b'a'; 1024], &[]).is_err());
        assert_eq!(integer_format(b"%d/%u", &[u32::MAX, 7]).unwrap(), b"-1/7");
    }
}
