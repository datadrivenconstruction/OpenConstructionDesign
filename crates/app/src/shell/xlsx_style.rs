//! The book every export writes: captions in the project language, key row, lists, formats.

use ddc_io::xlsx::Style;

/// Column keys that have a caption (`xlc.<key>` in the locale files).
pub const KEYS: [&str; 55] = ["along", "area", "ax", "ay", "base_offset", "bulge", "bx", "by", "category", "color", "conductivity", "count", "delete", "density", "elevation", "glass", "hatch", "height", "hinge", "id", "kind", "layers", "length", "level", "material", "mullions", "name", "note", "number", "offset", "offset0", "outline", "panels", "perimeter", "pos", "px", "py", "shape", "sill", "specific_heat", "stype", "swing", "thickness", "top_offset", "total", "type", "type_name", "ugrid", "vgrid", "volume", "wall", "width", "wtype", "x", "y"];

/// Captions in the catalog language of `locale` (English when it is unknown).
pub fn style(locale: &str) -> Style {
    Style::professional(
        KEYS.iter()
            .map(|k| (k.to_string(), crate::tr_locale(locale, &format!("xlc.{k}"), &[])))
            .collect(),
    )
    .with_number_format(locale.starts_with("ru"), locale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(lang: &str) -> std::collections::BTreeSet<String> {
        let p = format!("{}/../ui/data/i18n/{lang}/xlc.json", env!("CARGO_MANIFEST_DIR"));
        let v: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v.into_keys().collect()
    }

    #[test]
    fn captions_follow_locale_code() {
        for loc in ["en", "ru"] {
            let s = style(loc);
            for k in KEYS {
                assert!(!s.captions[k].starts_with("xlc."), "{loc}: {k} missing");
            }
        }
        assert_ne!(style("ru").captions["height"], style("en").captions["height"]);
        assert_eq!(style("ru-RU").captions["height"], style("ru").captions["height"]);
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        for loc in ["xx", "", "zz-ZZ"] {
            assert_eq!(style(loc).captions, style("en").captions, "{loc}");
        }
    }

    #[test]
    fn en_and_ru_xlc_have_same_keys() {
        assert_eq!(keys_of("en"), keys_of("ru"));
    }
}
