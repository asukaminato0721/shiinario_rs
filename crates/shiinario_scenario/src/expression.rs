//! Verified arithmetic subset shared by inline operands and command 03de.
use anyhow::{Context, Result, bail, ensure};

pub fn evaluate(bytes: &[u8], variable: impl Fn(&[u8]) -> Result<u32>) -> Result<u32> {
    Ok(evaluate_real(bytes, variable)? as i64 as u32)
}

pub fn evaluate_real(bytes: &[u8], variable: impl Fn(&[u8]) -> Result<u32>) -> Result<f64> {
    ensure!(bytes.len() <= 4096, "expression exceeds 4096 bytes");
    let mut parser = Parser {
        bytes,
        at: 0,
        variable,
    };
    let value = parser.sum(0)?;
    parser.spaces();
    ensure!(
        parser.at == bytes.len(),
        "unsupported expression syntax at byte {}",
        parser.at
    );
    Ok(value)
}
struct Parser<'a, F> {
    bytes: &'a [u8],
    at: usize,
    variable: F,
}
impl<F: Fn(&[u8]) -> Result<u32>> Parser<'_, F> {
    fn spaces(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t'))
        {
            self.at += 1;
        }
    }
    fn checked(value: f64) -> Result<f64> {
        ensure!(value.is_finite(), "nonfinite expression result");
        ensure!(
            value.abs() < (1u64 << 53) as f64,
            "expression exceeds supported numeric range"
        );
        Ok(value)
    }
    fn sum(&mut self, depth: usize) -> Result<f64> {
        let mut value = self.product(depth)?;
        loop {
            self.spaces();
            let op = self.bytes.get(self.at).copied();
            if !matches!(op, Some(b'+' | b'-')) {
                return Ok(value);
            }
            self.at += 1;
            let rhs = self.product(depth)?;
            value = Self::checked(if op == Some(b'+') {
                value + rhs
            } else {
                value - rhs
            })?;
        }
    }
    fn product(&mut self, depth: usize) -> Result<f64> {
        let mut value = self.atom(depth)?;
        loop {
            self.spaces();
            let op = self.bytes.get(self.at).copied();
            if !matches!(op, Some(b'*' | b'/')) {
                return Ok(value);
            }
            self.at += 1;
            let rhs = self.atom(depth)?;
            value = Self::checked(if op == Some(b'*') {
                value * rhs
            } else {
                ensure!(rhs != 0.0, "expression division by zero");
                value / rhs
            })?;
        }
    }
    fn atom(&mut self, depth: usize) -> Result<f64> {
        ensure!(depth < 64, "expression nesting exceeds 64");
        self.spaces();
        let byte = *self
            .bytes
            .get(self.at)
            .context("missing expression operand")?;
        self.at += 1;
        match byte {
            b'-' => Self::checked(-self.atom(depth + 1)?),
            b'(' => {
                let value = self.sum(depth + 1)?;
                self.spaces();
                ensure!(
                    self.bytes.get(self.at) == Some(&b')'),
                    "missing expression closing parenthesis"
                );
                self.at += 1;
                if self.bytes.get(self.at) == Some(&b'i') {
                    self.at += 1;
                    Ok(f64::from(value as i64 as i32))
                } else {
                    Ok(value)
                }
            }
            b'{' => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    self.at += 1;
                }
                ensure!(
                    self.at > start
                        && self.at - start <= 31
                        && self.bytes.get(self.at) == Some(&b'}'),
                    "unsupported expression variable syntax"
                );
                let name = &self.bytes[start..self.at];
                self.at += 1;
                let value = (self.variable)(name)?;
                let value = match self.bytes.get(self.at) {
                    Some(b'i') => {
                        self.at += 1;
                        f64::from(value as i32)
                    }
                    Some(b'f') => {
                        self.at += 1;
                        f64::from(f32::from_bits(value))
                    }
                    _ => f64::from(value),
                };
                Self::checked(value)
            }
            b'0'..=b'9' => {
                let mut value = f64::from(byte - b'0');
                loop {
                    self.spaces();
                    let Some(byte @ b'0'..=b'9') = self.bytes.get(self.at) else {
                        break;
                    };
                    value = Self::checked(value * 10.0 + f64::from(*byte - b'0'))?;
                    self.at += 1;
                }
                if self.bytes.get(self.at) == Some(&b'.') {
                    self.at += 1;
                    let mut place = 0.1;
                    loop {
                        self.spaces();
                        let Some(byte @ b'0'..=b'9') = self.bytes.get(self.at) else {
                            break;
                        };
                        value = Self::checked(value + f64::from(*byte - b'0') * place)?;
                        place *= 0.1;
                        self.at += 1;
                    }
                }
                Ok(value)
            }
            _ => bail!("unsupported expression operand at byte {}", self.at - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsupported_syntax_missing_variables_and_unbounded_work() {
        for bytes in [
            b"".as_slice(),
            b"1/0",
            b"1%2",
            b"{array[1]}",
            b"{missing}",
            b"1=2",
            b"1+",
            b"(1",
            b"9007199254740992+1",
        ] {
            assert!(evaluate(bytes, |_| bail!("undefined variable")).is_err());
        }
        assert!(evaluate(&vec![b'-'; 4097], |_| Ok(0)).is_err());
        assert!(
            evaluate(
                format!("{}1{}", "(".repeat(64), ")".repeat(64)).as_bytes(),
                |_| Ok(0)
            )
            .is_err()
        );
    }
}
