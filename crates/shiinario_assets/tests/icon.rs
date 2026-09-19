use shiinario_assets::icon::from_executable;

fn word(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn dword(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn executable(pe64: bool) -> Vec<u8> {
    let mut data = vec![0; 0x600];
    data[..2].copy_from_slice(b"MZ");
    dword(&mut data, 0x3c, 0x80);
    data[0x80..0x84].copy_from_slice(b"PE\0\0");
    word(&mut data, 0x84, if pe64 { 0x8664 } else { 0x14c });
    word(&mut data, 0x86, 1);
    let optional = 0x98;
    let optional_size = if pe64 { 0xf0 } else { 0xe0 };
    word(&mut data, 0x94, optional_size as u16);
    word(&mut data, optional, if pe64 { 0x20b } else { 0x10b });
    dword(&mut data, optional + 60, 0x200);
    let directories = optional + if pe64 { 112 } else { 96 };
    dword(&mut data, directories - 4, 16);
    dword(&mut data, directories + 16, 0x1000);
    dword(&mut data, directories + 20, 0x400);
    let section = optional + optional_size;
    data[section..section + 5].copy_from_slice(b".rsrc");
    for (offset, value) in [(8, 0x400), (12, 0x1000), (16, 0x400), (20, 0x200)] {
        dword(&mut data, section + offset, value);
    }
    let root = 0x200;
    for (table, entries) in [
        (0, vec![(3, 0x80000020), (14, 0x80000060)]),
        (0x20, vec![(7, 0x80000040)]),
        (0x40, vec![(0x409, 0xa0)]),
        (0x60, vec![(1, 0x80000080)]),
        (0x80, vec![(0x409, 0xb0)]),
    ] {
        word(&mut data, root + table + 14, entries.len() as u16);
        for (i, (id, target)) in entries.into_iter().enumerate() {
            dword(&mut data, root + table + 16 + i * 8, id);
            dword(&mut data, root + table + 20 + i * 8, target);
        }
    }
    dword(&mut data, root + 0xa0, 0x1100);
    dword(&mut data, root + 0xa4, 64);
    dword(&mut data, root + 0xb0, 0x1200);
    dword(&mut data, root + 0xb4, 20);
    let dib = root + 0x100;
    dword(&mut data, dib, 40);
    dword(&mut data, dib + 4, 2);
    dword(&mut data, dib + 8, 4);
    word(&mut data, dib + 12, 1);
    word(&mut data, dib + 14, 32);
    // Bottom-up BGRA with both partial and complete transparency.
    data[dib + 40..dib + 56].copy_from_slice(&[
        255, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255, 255, 0, 255, 0, 128,
    ]);
    let group = root + 0x200;
    word(&mut data, group + 2, 1);
    word(&mut data, group + 4, 1);
    data[group + 6..group + 8].copy_from_slice(&[2, 2]);
    word(&mut data, group + 10, 1);
    word(&mut data, group + 12, 32);
    dword(&mut data, group + 14, 64);
    word(&mut data, group + 18, 7);
    data
}

#[test]
fn pe32_and_pe64_icons_preserve_color_orientation_and_alpha() {
    for pe64 in [false, true] {
        let icon = from_executable(&executable(pe64)).unwrap().unwrap();
        assert_eq!([icon.width, icon.height], [2, 2]);
        assert_eq!(
            icon.rgba,
            [
                255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 255, 0
            ]
        );
    }
}

#[test]
fn missing_icons_and_malformed_resource_offsets_are_bounded() {
    let good = executable(false);
    for length in [0, 2, 0x3f, 0x90, 0x200, 0x240, 0x301] {
        assert!(
            from_executable(&good[..length]).is_err(),
            "truncation at {length}"
        );
    }
    let mut bad = good.clone();
    dword(&mut bad, 0x2a0, 0xfffffff0);
    assert!(from_executable(&bad).is_err());
    let mut bad = good.clone();
    word(&mut bad, 0x412, 99);
    assert!(from_executable(&bad).is_err());
    let mut empty = good;
    word(&mut empty, 0x20e, 0);
    assert!(from_executable(&empty).unwrap().is_none());
}
