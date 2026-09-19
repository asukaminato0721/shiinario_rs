//! Verified integer subset of inline expression operands (tag 11).
use anyhow::{Context, Result, bail, ensure};

pub fn evaluate(bytes: &[u8], variable: impl Fn(&[u8]) -> Result<u32>) -> Result<u32> {
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
    Ok(value as u32)
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
    fn checked(value: Option<i64>) -> Result<i64> {
        let value = value.context("expression integer overflow")?;
        // These integers are exact in the native parser's double temporaries.
        ensure!(
            value.unsigned_abs() <= (1u64 << 53),
            "expression exceeds exact integer range"
        );
        Ok(value)
    }
    fn sum(&mut self, depth: usize) -> Result<i64> {
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
                value.checked_add(rhs)
            } else {
                value.checked_sub(rhs)
            })?;
        }
    }
    fn product(&mut self, depth: usize) -> Result<i64> {
        let mut value = self.atom(depth)?;
        loop {
            self.spaces();
            if self.bytes.get(self.at) != Some(&b'*') {
                return Ok(value);
            }
            self.at += 1;
            value = Self::checked(value.checked_mul(self.atom(depth)?))?;
        }
    }
    fn atom(&mut self, depth: usize) -> Result<i64> {
        ensure!(depth < 64, "expression nesting exceeds 64");
        self.spaces();
        let byte = *self
            .bytes
            .get(self.at)
            .context("missing expression operand")?;
        self.at += 1;
        match byte {
            b'-' => Self::checked(self.atom(depth + 1)?.checked_neg()),
            b'(' => {
                let value = self.sum(depth + 1)?;
                self.spaces();
                ensure!(
                    self.bytes.get(self.at) == Some(&b')'),
                    "missing expression closing parenthesis"
                );
                self.at += 1;
                Ok(value)
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
                Ok(i64::from((self.variable)(name)?))
            }
            b'0'..=b'9' => {
                let mut value = i64::from(byte - b'0');
                loop {
                    self.spaces();
                    let Some(byte @ b'0'..=b'9') = self.bytes.get(self.at) else {
                        break;
                    };
                    value = Self::checked(
                        value
                            .checked_mul(10)
                            .and_then(|v| v.checked_add(i64::from(*byte - b'0'))),
                    )?;
                    self.at += 1;
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
            b"1/2",
            b"1.5",
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
