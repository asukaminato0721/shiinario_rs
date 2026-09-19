//! A synthetic GARbro/NRBF catalog. No external database is needed by default tests.
use std::io::Write;

pub struct Writer(pub Vec<u8>);
impl Writer {
    pub fn int(&mut self, n: i32) {
        self.0.extend(n.to_le_bytes());
    }
    pub fn string(&mut self, text: &str) {
        let mut len = text.len();
        while len >= 128 {
            self.0.push((len as u8) | 128);
            len >>= 7;
        }
        self.0.push(len as u8);
        self.0.extend(text.as_bytes());
    }
    pub fn object(&mut self, id: i32, name: &str, fields: &[(&str, Option<u8>)]) {
        self.0.push(5);
        self.int(id);
        self.string(name);
        self.int(fields.len() as i32);
        for (name, _) in fields {
            self.string(name);
        }
        for (_, ty) in fields {
            self.0.push(if ty.is_some() { 0 } else { 2 });
        }
        for (_, ty) in fields {
            if let Some(ty) = ty {
                self.0.push(*ty);
            }
        }
        self.int(1);
    }
    pub fn text(&mut self, id: i32, value: &str) {
        self.0.push(6);
        self.int(id);
        self.string(value);
    }
    pub fn reference(&mut self, id: i32) {
        self.0.push(9);
        self.int(id);
    }
    pub fn array(&mut self, id: i32, count: i32) {
        self.0.push(16);
        self.int(id);
        self.int(count);
    }
    pub fn bytes(&mut self, id: i32, bytes: &[u8]) {
        self.0.push(15);
        self.int(id);
        self.int(bytes.len() as i32);
        self.0.push(2);
        self.0.extend(bytes);
    }
}

pub fn stream() -> Vec<u8> {
    let mut w = Writer(vec![0]);
    w.int(1);
    w.int(-1);
    w.int(1);
    w.int(0);
    w.0.push(12);
    w.int(1);
    w.string("Synthetic.GARbro");
    w.object(
        1,
        "GameRes.SchemeDataBase",
        &[("Version", Some(8)), ("SchemeMap", None)],
    );
    w.int(148);
    w.object(
        2,
        "System.Collections.Generic.Dictionary`2",
        &[("KeyValuePairs", None)],
    );
    w.array(3, 1);
    w.object(
        4,
        "System.Collections.Generic.KeyValuePair`2",
        &[("key", None), ("value", None)],
    );
    w.text(5, "WAR");
    w.object(
        6,
        "GameRes.Formats.ShiinaRio.WarcScheme",
        &[("KnownSchemes", None)],
    );
    // Forward reference, as in the real catalog.
    w.reference(7);
    w.array(7, 2);
    w.object(
        8,
        "GameRes.Formats.ShiinaRio.EncryptionScheme",
        &[
            ("<Name>k__BackingField", None),
            ("<Version>k__BackingField", Some(8)),
            ("EntryNameSize", Some(8)),
            ("ExtraCrypt", None),
            ("CryptKey", None),
            ("Region", None),
            ("DecodeBin", None),
            ("HelperKey", None),
            ("ShiinaImage", None),
        ],
    );
    for (id, title) in [(8, "Unrelated"), (10, "Ran→Sem")] {
        if id == 10 {
            w.0.push(1);
            w.int(id);
            w.int(8);
        } // Reuse class metadata.
        w.text(id + 1, title);
        w.int(2470);
        w.int(32);
        w.0.push(10);
        for id in 12..=16 {
            w.reference(id);
        }
    }
    // The archive fixture's index uses this fixed CP932 text key. Its payload
    // is unencrypted, so the remaining tables can contain synthetic data.
    w.bytes(
        12,
        b"Crypt Type 20011002 - Copyright(C) 2000 Y.Yamada/STUDIO \x82\xe6\x82\xb5\x82\xad\x82\xf1",
    );
    w.bytes(13, &[0; 9216]);
    w.bytes(14, &[0; 8192]);
    w.0.push(15);
    w.int(15);
    w.int(5);
    w.0.push(15);
    for key in [1667458680u32, 1719289936, 1349942115, 20061, 0] {
        w.0.extend(key.to_le_bytes());
    }
    w.object(
        16,
        "GameRes.Formats.ShiinaRio.ImageArray",
        &[
            ("m_common", None),
            ("m_common_length", Some(8)),
            ("m_extra", None),
        ],
    );
    w.reference(17);
    w.int(1);
    w.reference(18);
    w.bytes(17, &[0x11, 0xfe]);
    w.bytes(18, &[0x22, 0x33]);
    w.0.push(11);
    w.0
}

pub fn wrap(raw: &[u8]) -> Vec<u8> {
    let mut data = b"GARbroDB".to_vec();
    data.extend(148i32.to_le_bytes());
    let mut encoder = flate2::write::ZlibEncoder::new(data, flate2::Compression::default());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}

pub fn database() -> Vec<u8> {
    wrap(&stream())
}
