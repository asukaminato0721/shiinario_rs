//! Read the original game's PE icon resources without executing Windows code.
use anyhow::{Context, Result, ensure};
use object::{
    LittleEndian as LE,
    read::pe::{ImageNtHeaders, PeFile, ResourceDirectory, ResourceNameOrId},
};
use std::{
    io::{Cursor, Read},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn from_game(directory: &Path) -> Result<Option<Icon>> {
    for path in crate::profile::Catalog::builtin()?.game_executables(directory)? {
        let mut bytes = Vec::new();
        crate::fs::File::open(&path)?
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 128 * 1024 * 1024,
            "executable exceeds icon reader size limit"
        );
        if let Some(icon) = from_executable(&bytes)
            .with_context(|| format!("reading icon from {}", path.display()))?
        {
            return Ok(Some(icon));
        }
    }
    Ok(None)
}

pub fn from_executable(bytes: &[u8]) -> Result<Option<Icon>> {
    match object::FileKind::parse(bytes)? {
        object::FileKind::Pe32 => from_pe(
            &PeFile::<object::pe::ImageNtHeaders32>::parse(bytes)?,
            bytes,
        ),
        object::FileKind::Pe64 => from_pe(
            &PeFile::<object::pe::ImageNtHeaders64>::parse(bytes)?,
            bytes,
        ),
        _ => anyhow::bail!("icon source is not a PE executable"),
    }
}

fn resource<'a>(
    directory: ResourceDirectory<'a>,
    kind: u16,
    id: Option<u16>,
    language: Option<u16>,
) -> Result<Option<(&'a object::pe::ImageResourceDataEntry, u16)>> {
    let root = directory.root()?;
    let Some(entry) = root
        .entries
        .iter()
        .find(|entry| matches!(entry.name_or_id(), ResourceNameOrId::Id(n) if n == kind))
    else {
        return Ok(None);
    };
    let names = entry
        .data(directory)?
        .table()
        .context("invalid icon resource type table")?;
    let Some(entry) = names.entries.iter().find(|entry| {
        id.is_none_or(|id| matches!(entry.name_or_id(), ResourceNameOrId::Id(n) if n == id))
    }) else {
        return Ok(None);
    };
    let languages = entry
        .data(directory)?
        .table()
        .context("invalid icon resource name table")?;
    let entry = languages
        .entries
        .iter()
        .find(|entry| {
            language
                .is_some_and(|id| matches!(entry.name_or_id(), ResourceNameOrId::Id(n) if n == id))
        })
        .or_else(|| languages.entries.first())
        .context("empty icon language table")?;
    let ResourceNameOrId::Id(language) = entry.name_or_id() else {
        anyhow::bail!("invalid icon language")
    };
    Ok(Some((
        entry
            .data(directory)?
            .data()
            .context("invalid icon resource data")?,
        language,
    )))
}

fn from_pe<Pe: ImageNtHeaders>(pe: &PeFile<'_, Pe>, bytes: &[u8]) -> Result<Option<Icon>> {
    let sections = pe.section_table();
    let Some(directory) = pe.data_directories().resource_directory(bytes, &sections)? else {
        return Ok(None);
    };
    let Some((group, language)) = resource(directory, 14, None, None)? else {
        return Ok(None);
    };
    let data = |entry: &object::pe::ImageResourceDataEntry| -> Result<&[u8]> {
        let size = entry.size.get(LE) as usize;
        ensure!(size <= 4 * 1024 * 1024, "icon resource exceeds size limit");
        sections
            .pe_data_at(bytes, entry.offset_to_data.get(LE))
            .and_then(|data| data.get(..size))
            .context("icon resource outside PE sections")
    };
    let group = data(group)?;
    ensure!(
        group.len() >= 6 && group[..4] == [0, 0, 1, 0],
        "invalid icon group header"
    );
    let count = u16::from_le_bytes(group[4..6].try_into()?) as usize;
    ensure!(
        count > 0 && count <= 256 && group.len() >= 6 + count * 14,
        "invalid icon group entries"
    );
    // The best original resolution is also used on high-DPI displays.
    let dimension = |n: u8| if n == 0 { 256 } else { u32::from(n) };
    let entry = group[6..6 + count * 14]
        .as_chunks::<14>()
        .0
        .iter()
        .max_by_key(|entry| {
            (
                dimension(entry[0]) * dimension(entry[1]),
                u16::from_le_bytes([entry[6], entry[7]]),
            )
        })
        .unwrap();
    let id = u16::from_le_bytes(entry[12..14].try_into()?);
    let (image, _) =
        resource(directory, 3, Some(id), Some(language))?.context("missing icon image resource")?;
    let image = data(image)?;
    // Convert the selected GRPICONDIRENTRY to an ordinary one-entry ICO file.
    let mut ico = vec![0, 0, 1, 0, 1, 0];
    ico.extend_from_slice(&entry[..8]);
    ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(image);
    let mut reader = ::image::ImageReader::with_format(Cursor::new(ico), ::image::ImageFormat::Ico);
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(256);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.into_rgba8();
    // The Wayland icon protocol requires a square buffer. Preserve any unusual
    // rectangular source with transparent padding instead of distorting it.
    let side = image.width().max(image.height());
    let mut rgba = vec![0; side as usize * side as usize * 4];
    let x = (side - image.width()) as usize / 2;
    let y = (side - image.height()) as usize / 2;
    for (row, pixels) in image
        .as_raw()
        .chunks_exact(image.width() as usize * 4)
        .enumerate()
    {
        let start = ((row + y) * side as usize + x) * 4;
        rgba[start..start + pixels.len()].copy_from_slice(pixels);
    }
    Ok(Some(Icon {
        width: side,
        height: side,
        rgba,
    }))
}
