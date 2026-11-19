//! The one file-size formatter: unit names and decimal mark follow the catalog language.

use ddc_ui::widgets::NumberFormat;
use ddc_ui::Catalog;

const KEYS: [&str; 4] = ["unit.size_b", "unit.size_kb", "unit.size_mb", "unit.size_gb"];

/// `512 B`, `1.5 KB`, `3.0 MB`, `2.0 GB`; Russian reads `3,0 MB` with its own unit names.
/// Whole bytes below 1 KB, one decimal above; a value that rounds up to 1024 moves to the next unit.
pub fn fmt_size(c: &Catalog, bytes: u64) -> String {
    if bytes < 1024 {
        return c.tf(KEYS[0], &[("n", bytes.to_string().as_str())]);
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < KEYS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i + 1 < KEYS.len() && (v * 10.0).round() >= 10_240.0 {
        v /= 1024.0;
        i += 1;
    }
    let mark = NumberFormat::for_lang(c.lang).decimal.to_string();
    let n = format!("{v:.1}").replace('.', &mark);
    c.tf(KEYS[i], &[("n", n.as_str())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn en(b: u64) -> String {
        fmt_size(&Catalog::new(Lang::En), b)
    }
    fn ru(b: u64) -> String {
        fmt_size(&Catalog::new(Lang::Ru), b)
    }

    #[test]
    fn english_uses_the_point_and_all_four_units() {
        assert_eq!(en(0), "0 B");
        assert_eq!(en(1023), "1023 B");
        assert_eq!(en(1024), "1.0 KB");
        assert_eq!(en(1536), "1.5 KB");
        assert_eq!(en(3 << 20), "3.0 MB");
        assert_eq!(en(3 << 30), "3.0 GB");
        assert_eq!(en(5 << 40), "5120.0 GB");
    }

    #[test]
    fn russian_uses_the_comma_and_russian_units() {
        assert_eq!(ru(10), "10 \u{411}");
        assert_eq!(ru(1536), "1,5 \u{41a}\u{411}");
        assert_eq!(ru(3 << 20), "3,0 \u{41c}\u{411}");
        assert_eq!(ru(3 << 30), "3,0 \u{413}\u{411}");
    }

    #[test]
    fn boundaries_step_to_the_next_unit() {
        assert_eq!(en((1 << 20) - 1), "1.0 MB");
        assert_eq!(en(1 << 20), "1.0 MB");
        assert_eq!(en((1 << 30) - 1), "1.0 GB");
        assert_eq!(en((1 << 20) - 1024), "1023.0 KB");
        assert_eq!(ru((1 << 20) - 1), "1,0 \u{41c}\u{411}");
    }

    #[test]
    fn no_other_size_formatter_is_left() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        let mut stack = vec![root];
        let needle = ["fn fmt", "_size("].concat();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let s = std::fs::read_to_string(&p).unwrap_or_default();
                    if s.contains(&needle) {
                        hits.push(p.display().to_string());
                    }
                }
            }
        }
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].replace('\\', "/").ends_with("ui_kit/size.rs"), "{hits:?}");
    }
}
