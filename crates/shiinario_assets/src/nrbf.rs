//! Data-only reader for the typed NRBF records used by GARbro Formats.dat.
//! No .NET types are instantiated and no deserialization callbacks are run.
//! Record definitions: https://learn.microsoft.com/openspecs/windows_protocols/ms-nrbf/
use anyhow::{Context, Result, bail, ensure};
use std::collections::{HashMap, HashSet};

const MAX_ITEMS: usize = 1_000_000;
const MAX_DEPTH: usize = 128;

#[derive(Debug)]
pub(crate) enum Value<'a> {
    Null,
    Number(i128),
    OtherPrimitive,
    String(&'a str),
    Bytes(&'a [u8]),
    Array(Vec<Value<'a>>),
    Object(&'a str, Vec<(&'a str, Value<'a>)>),
    Ref(i32),
}

pub(crate) struct Document<'a> {
    root: Value<'a>,
    objects: HashMap<i32, Value<'a>>,
}

impl<'a> Document<'a> {
    pub fn root(&self) -> Result<&Value<'a>> {
        self.resolve(&self.root)
    }

    pub fn resolve<'s>(&'s self, mut value: &'s Value<'a>) -> Result<&'s Value<'a>> {
        for _ in 0..MAX_DEPTH {
            match value {
                Value::Ref(id) => value = self.objects.get(id).context("missing NRBF reference")?,
                _ => return Ok(value),
            }
        }
        bail!("cyclic or excessively deep NRBF reference")
    }

    pub fn field<'s>(&'s self, value: &'s Value<'a>, name: &str) -> Result<&'s Value<'a>> {
        let Value::Object(_, fields) = self.resolve(value)? else {
            bail!("expected NRBF object for field {name}");
        };
        let (_, value) = fields
            .iter()
            .find(|(key, _)| *key == name)
            .with_context(|| format!("missing NRBF field {name}"))?;
        self.resolve(value)
    }

    pub fn class(&self, value: &Value<'a>, expected: &str) -> Result<()> {
        ensure!(
            matches!(self.resolve(value)?, Value::Object(name, _) if *name == expected),
            "expected NRBF class {expected}"
        );
        Ok(())
    }

    pub fn array<'s>(&'s self, value: &'s Value<'a>) -> Result<&'s [Value<'a>]> {
        match self.resolve(value)? {
            Value::Array(values) => Ok(values),
            _ => bail!("expected NRBF array"),
        }
    }

    pub fn bytes(&self, value: &Value<'a>) -> Result<&'a [u8]> {
        match self.resolve(value)? {
            Value::Bytes(bytes) => Ok(bytes),
            _ => bail!("expected NRBF byte array"),
        }
    }

    pub fn string(&self, value: &Value<'a>) -> Result<&'a str> {
        match self.resolve(value)? {
            Value::String(text) => Ok(text),
            _ => bail!("expected NRBF string"),
        }
    }

    pub fn number(&self, value: &Value<'a>) -> Result<i128> {
        match self.resolve(value)? {
            Value::Number(number) => Ok(*number),
            _ => bail!("expected NRBF integer"),
        }
    }
}

#[derive(Clone)]
struct Metadata<'a> {
    name: &'a str,
    fields: Vec<(&'a str, Option<u8>)>,
}

struct Reader<'a> {
    remaining: &'a [u8],
    objects: HashMap<i32, Value<'a>>,
    ids: HashSet<i32>,
    references: Vec<i32>,
    metadata: HashMap<i32, Metadata<'a>>,
    items_left: usize,
}

pub(crate) fn parse(data: &[u8]) -> Result<Document<'_>> {
    let mut reader = Reader {
        remaining: data,
        objects: HashMap::new(),
        ids: HashSet::new(),
        references: Vec::new(),
        metadata: HashMap::new(),
        items_left: MAX_ITEMS,
    };
    ensure!(reader.byte()? == 0, "missing NRBF stream header");
    let root = reader.int()?;
    reader.int()?; // Header ID is unused by GARbro.
    ensure!(
        reader.int()? == 1 && reader.int()? == 0,
        "unsupported NRBF version"
    );
    loop {
        if reader.remaining.first() == Some(&11) {
            reader.byte()?;
            break;
        }
        reader.value(0)?;
    }
    ensure!(reader.remaining.is_empty(), "trailing NRBF data");
    ensure!(
        reader.objects.contains_key(&root),
        "missing NRBF root object"
    );
    for id in reader.references {
        ensure!(
            reader.objects.contains_key(&id),
            "missing NRBF reference {id}"
        );
    }
    Ok(Document {
        root: Value::Ref(root),
        objects: reader.objects,
    })
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.remaining.len(), "truncated NRBF data");
        let (data, rest) = self.remaining.split_at(n);
        self.remaining = rest;
        Ok(data)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn int(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn count(&mut self) -> Result<usize> {
        let n = self.int()?;
        ensure!(
            n >= 0 && n as usize <= MAX_ITEMS,
            "NRBF count exceeds limit"
        );
        Ok(n as usize)
    }

    fn charge(&mut self, count: usize) -> Result<()> {
        self.items_left = self
            .items_left
            .checked_sub(count)
            .context("NRBF item budget exceeded")?;
        Ok(())
    }

    fn id(&mut self) -> Result<i32> {
        let id = self.int()?;
        ensure!(
            id != 0 && self.ids.insert(id),
            "invalid or duplicate NRBF object ID {id}"
        );
        Ok(id)
    }

    fn string(&mut self) -> Result<&'a str> {
        let mut length = 0usize;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            ensure!(shift < 28 || byte <= 7, "invalid NRBF string length");
            length |= ((byte & 127) as usize) << shift;
            if byte & 128 == 0 {
                ensure!(length <= 1024 * 1024, "NRBF string exceeds limit");
                return Ok(std::str::from_utf8(self.take(length)?)?);
            }
        }
        bail!("invalid NRBF string length")
    }

    // Return the primitive tag only for inline primitive values. Other types
    // use a record, but their additional type information must still be read.
    fn member_type(&mut self, tag: u8) -> Result<Option<u8>> {
        match tag {
            0 => Ok(Some(self.byte()?)),
            1 | 2 | 5 | 6 => Ok(None),
            3 => {
                self.string()?;
                Ok(None)
            }
            4 => {
                self.string()?;
                self.int()?;
                Ok(None)
            }
            7 => {
                self.byte()?;
                Ok(None)
            }
            _ => bail!("unsupported NRBF binary type {tag}"),
        }
    }

    fn primitive(&mut self, tag: u8) -> Result<Value<'a>> {
        let n = match tag {
            1 => {
                let b = self.byte()?;
                ensure!(b <= 1, "invalid NRBF boolean");
                b as i128
            }
            2 => self.byte()? as i128,
            3 => {
                let first = *self.remaining.first().context("truncated NRBF char")?;
                let size = match first {
                    0..=127 => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => bail!("invalid NRBF char"),
                };
                std::str::from_utf8(self.take(size)?)?;
                return Ok(Value::OtherPrimitive);
            }
            5 => {
                self.string()?;
                return Ok(Value::OtherPrimitive);
            }
            6 | 13 => {
                self.take(8)?;
                return Ok(Value::OtherPrimitive);
            }
            7 => i16::from_le_bytes(self.take(2)?.try_into().unwrap()) as i128,
            8 => self.int()? as i128,
            9 | 12 => i64::from_le_bytes(self.take(8)?.try_into().unwrap()) as i128,
            10 => self.byte()? as i8 as i128,
            11 => {
                self.take(4)?;
                return Ok(Value::OtherPrimitive);
            }
            14 => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as i128,
            15 => u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as i128,
            16 => u64::from_le_bytes(self.take(8)?.try_into().unwrap()) as i128,
            _ => bail!("unsupported NRBF primitive {tag}"),
        };
        Ok(Value::Number(n))
    }

    fn elements(&mut self, count: usize, primitive: Option<u8>, depth: usize) -> Result<Value<'a>> {
        if primitive == Some(2) {
            return Ok(Value::Bytes(self.take(count)?));
        }
        self.charge(count)?;
        let mut values = Vec::with_capacity(count);
        while values.len() < count {
            if let Some(tag) = primitive {
                values.push(self.primitive(tag)?);
            } else if matches!(self.remaining.first(), Some(13 | 14)) {
                let tag = self.byte()?;
                let run = if tag == 13 {
                    self.byte()? as usize
                } else {
                    self.count()?
                };
                ensure!(
                    run > 0 && run <= count - values.len(),
                    "invalid NRBF null run"
                );
                values.extend((0..run).map(|_| Value::Null));
            } else {
                values.push(self.value(depth + 1)?);
            }
        }
        Ok(Value::Array(values))
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>> {
        ensure!(depth < MAX_DEPTH, "NRBF nesting exceeds limit");
        self.charge(1)?;
        let tag = loop {
            let tag = self.byte()?;
            if tag != 12 {
                break tag;
            }
            self.int()?;
            self.string()?;
        };
        match tag {
            1 | 4 | 5 => {
                let id = self.id()?;
                let metadata = if tag == 1 {
                    let mid = self.int()?;
                    self.metadata
                        .get(&mid)
                        .context("missing NRBF class metadata")?
                        .clone()
                } else {
                    let name = self.string()?;
                    let count = self.count()?;
                    ensure!(count <= 1024, "too many NRBF class members");
                    self.charge(count)?;
                    let names = (0..count)
                        .map(|_| self.string())
                        .collect::<Result<Vec<_>>>()?;
                    ensure!(
                        names.iter().copied().collect::<HashSet<_>>().len() == count,
                        "duplicate NRBF member"
                    );
                    let tags = self.take(count)?.to_vec();
                    let mut fields = Vec::with_capacity(count);
                    for (name, tag) in names.into_iter().zip(tags) {
                        fields.push((name, self.member_type(tag)?));
                    }
                    if tag == 5 {
                        self.int()?;
                    }
                    let metadata = Metadata { name, fields };
                    self.metadata.insert(id, metadata.clone());
                    metadata
                };
                self.charge(metadata.fields.len())?;
                let mut fields = Vec::with_capacity(metadata.fields.len());
                for (name, primitive) in metadata.fields {
                    let value = match primitive {
                        Some(tag) => self.primitive(tag)?,
                        None => self.value(depth + 1)?,
                    };
                    fields.push((name, value));
                }
                self.objects
                    .insert(id, Value::Object(metadata.name, fields));
                Ok(Value::Ref(id))
            }
            6 => {
                let id = self.id()?;
                let text = self.string()?;
                self.objects.insert(id, Value::String(text));
                Ok(Value::Ref(id))
            }
            7 | 15 | 16 | 17 => {
                let id = self.id()?;
                let (count, primitive) = if tag == 7 {
                    let kind = self.byte()?;
                    ensure!(kind <= 5, "invalid NRBF array kind");
                    let rank = self.count()?;
                    ensure!(rank > 0 && rank <= 32, "invalid NRBF array rank");
                    ensure!(
                        matches!(kind, 2 | 5) || rank == 1,
                        "invalid NRBF single array rank"
                    );
                    let mut count = 1usize;
                    for _ in 0..rank {
                        count = count
                            .checked_mul(self.count()?)
                            .context("NRBF array length overflow")?;
                        ensure!(count <= MAX_ITEMS, "NRBF array exceeds limit");
                    }
                    if kind >= 3 {
                        for _ in 0..rank {
                            self.int()?;
                        }
                    }
                    let binary_type = self.byte()?;
                    (count, self.member_type(binary_type)?)
                } else {
                    let count = self.count()?;
                    (count, if tag == 15 { Some(self.byte()?) } else { None })
                };
                let value = self.elements(count, primitive, depth)?;
                self.objects.insert(id, value);
                Ok(Value::Ref(id))
            }
            8 => {
                let tag = self.byte()?;
                self.primitive(tag)
            }
            9 => {
                let id = self.int()?;
                self.references.push(id);
                Ok(Value::Ref(id))
            }
            10 => Ok(Value::Null),
            _ => bail!("unsupported NRBF record {tag}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Writer;

    fn header() -> Writer {
        let mut w = Writer(vec![0]);
        w.int(1);
        w.int(-1);
        w.int(1);
        w.int(0);
        w
    }

    #[test]
    fn binary_arrays_and_null_runs() {
        let mut w = header();
        w.0.push(7);
        w.int(1);
        w.0.push(0);
        w.int(1);
        w.int(4);
        w.0.push(2);
        w.0.extend([13, 2, 14]);
        w.int(2);
        w.0.push(11);
        let doc = parse(&w.0).unwrap();
        assert!(
            doc.array(doc.root().unwrap())
                .unwrap()
                .iter()
                .all(|v| matches!(v, Value::Null))
        );
        for at in 17..w.0.len() {
            assert!(parse(&w.0[..at]).is_err());
        }
        let mut w = header();
        w.0.push(7);
        w.int(1);
        w.0.push(0);
        w.int(1);
        w.int(3);
        w.0.extend([0, 2, 1, 2, 3, 11]);
        let doc = parse(&w.0).unwrap();
        assert_eq!(doc.bytes(doc.root().unwrap()).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn rejects_invalid_ids_lengths_runs_and_records() {
        let cases: &[&[u8]] = &[
            &[16, 1, 0, 0, 0, 1, 0, 0, 0, 9, 2, 0, 0, 0, 11], // Dangling reference.
            &[16, 1, 0, 0, 0, 1, 0, 0, 0, 13, 2, 11],         // Null run beyond array.
            &[16, 1, 0, 0, 0, 1, 0, 0, 0, 13, 0, 11],         // Empty null run.
            &[16, 1, 0, 0, 0, 255, 255, 255, 255, 11],        // Negative count.
            &[16, 1, 0, 0, 0, 255, 255, 255, 127, 11],        // Excessive count.
            &[6, 1, 0, 0, 0, 0, 6, 1, 0, 0, 0, 0, 11],        // Duplicate ID.
            &[6, 1, 0, 0, 0, 255, 255, 255, 255, 255],        // Bad string length.
            &[1, 1, 0, 0, 0, 2, 0, 0, 0, 11],                 // Missing class metadata.
            &[21, 11],                                        // Method calls are not data records.
        ];
        for case in cases {
            let mut w = header();
            w.0.extend(*case);
            assert!(parse(&w.0).is_err(), "accepted {case:?}");
        }
        let mut w = header();
        for id in 1..=130 {
            w.array(id, 1);
        }
        w.0.extend([10, 11]);
        assert!(parse(&w.0).is_err());
    }
}
