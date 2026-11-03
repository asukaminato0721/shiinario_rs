use ab_glyph::{Font, FontArc, FontRef, FontVec};
use anyhow::{Context, Result, ensure};
use fontdb::{Database, Family, Query, Weight};
use std::collections::BTreeSet;

const TRADITIONAL: &[&str] = &[
    "MingLiU",
    "PMingLiU",
    "MingLiU_HKSCS",
    "Microsoft JhengHei",
    "Microsoft JhengHei UI",
    "MingLiU-ExtB",
    "PMingLiU-ExtB",
    "MingLiU_HKSCS-ExtB",
    "Noto Serif CJK TC",
    "Noto Sans CJK TC",
    "Noto Serif TC",
    "Noto Sans TC",
    "Source Han Serif TC",
    "Source Han Sans TC",
];
const JAPANESE: &[&str] = &[
    "MS Gothic",
    "Yu Gothic",
    "Meiryo",
    "Yu Gothic UI",
    "MS Mincho",
    "Yu Mincho",
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Source Han Sans JP",
];

#[cfg(target_os = "windows")]
pub(super) fn installed() -> &'static Database {
    static FONTS: std::sync::OnceLock<Database> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = Database::new();
        fonts.load_system_fonts();
        fonts
    })
}

pub(super) fn load(
    fonts: &Database,
    name: &str,
    weight: u32,
    character: Option<char>,
) -> Result<FontArc> {
    ensure!(weight <= 1000, "text font weight exceeds 1000");
    let weight = Weight(weight as u16);
    let lower = name.to_ascii_lowercase();
    let traditional = [
        "mingliu",
        "jhenghei",
        " cjk tc",
        " sans tc",
        " serif tc",
        "明體",
        "正黑體",
    ]
    .iter()
    .any(|part| lower.contains(part));
    let groups = if traditional {
        [TRADITIONAL, JAPANESE]
    } else {
        [JAPANESE, TRADITIONAL]
    };
    let mut seen = BTreeSet::new();
    let named = std::iter::once(name)
        .chain(groups.into_iter().flatten().copied())
        .filter_map(|name| {
            // Fontdb queries require the stored spelling, including localized names.
            let family = fonts
                .faces()
                .flat_map(|face| &face.families)
                .find(|(family, _)| family.eq_ignore_ascii_case(name))?;
            fonts.query(&Query {
                families: &[Family::Name(&family.0)],
                weight,
                ..Default::default()
            })
        });
    // A font outside the preferred families can supply a rare character.
    // Keep weight matching for these candidates as well.
    let mut remaining = Vec::new();
    if character.is_some() {
        remaining.extend(fonts.faces());
        remaining.sort_by_key(|face| {
            (
                face.style != fontdb::Style::Normal,
                face.weight.0.abs_diff(weight.0),
            )
        });
    }
    let selected = named
        .chain(remaining.into_iter().map(|face| face.id))
        .filter(|id| seen.insert(*id))
        .find_map(|id| {
            let font = fonts.with_face_data(id, |bytes, index| {
                if bytes.len() > 32 * 1024 * 1024 {
                    return None;
                }
                let font = FontRef::try_from_slice_and_index(bytes, index).ok()?;
                if character.is_some_and(|character| font.glyph_id(character).0 == 0) {
                    return None;
                }
                FontVec::try_from_vec_and_index(bytes.to_vec(), index)
                    .ok()
                    .map(FontArc::from)
            })??;
            Some((id, font))
        });
    let (id, font) = selected.with_context(|| match character {
        Some(character) => format!(
            "No usable installed font has glyph {character:?} (U+{:04X}). Install a font that contains this character.",
            character as u32
        ),
        None => "No usable text font found. Install MingLiU, Microsoft JhengHei, MS Gothic, or a Noto CJK font.".to_owned(),
    })?;
    let face = fonts.face(id).context("selected Windows font is missing")?;
    eprintln!(
        "Text font: {name:?} -> {} (face {}, character {character:?})",
        face.post_script_name, face.index
    );
    Ok(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> Database {
        let mut fonts = Database::new();
        fonts.load_font_data(include_bytes!("../../tests/fixtures/font-fallback.ttc").to_vec());
        assert_eq!(fonts.faces().count(), 3);
        fonts
    }

    #[test]
    fn traditional_family_fallback_and_weight_select_the_correct_ttc_face() {
        let fonts = fonts();
        let regular = load(&fonts, "MingLiU", 400, None).unwrap();
        let bold = load(&fonts, "mingliu", 900, None).unwrap();
        assert_ne!(regular.glyph_id('說').0, 0);
        assert_ne!(bold.glyph_id('說').0, 0);
        assert_eq!(regular.h_advance_unscaled(regular.glyph_id('說')), 500.0);
        assert_eq!(bold.h_advance_unscaled(bold.glyph_id('說')), 700.0);
    }

    #[test]
    fn missing_glyph_uses_a_cached_fallback_without_replacing_the_primary() {
        let fonts = fonts();
        let primary = load(&fonts, "ms gothic", 400, None).unwrap();
        assert_eq!(primary.glyph_id('說').0, 0);
        let mut face = crate::text_render::Face {
            name: b"MS Gothic".to_vec(),
            weight: 400,
            font: primary,
            fallbacks: Vec::new(),
        };
        let fallback = face
            .font_for('說', |_, weight, character| {
                load(&fonts, "MS Gothic", weight, Some(character))
            })
            .unwrap();
        assert_ne!(fallback.glyph_id('說').0, 0);
        assert!(
            fallback
                .outline_glyph(fallback.glyph_id('說').with_scale(24.0))
                .is_some()
        );
        for character in ['說', '𠀀', 'A'] {
            let font = face
                .font_for(character, |_, _, _| {
                    panic!("cached font must cover the character")
                })
                .unwrap();
            assert_ne!(font.glyph_id(character).0, 0);
        }
        assert_eq!(face.fallbacks.len(), 1);
        assert_eq!(face.font.glyph_id('說').0, 0);
        let error = load(&fonts, "MS Gothic", 400, Some('龘')).err().unwrap();
        assert!(error.to_string().contains("U+9F98"));
    }
}
