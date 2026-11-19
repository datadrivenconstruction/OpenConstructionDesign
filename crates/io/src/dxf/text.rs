//! Display text of TEXT and MTEXT values: control codes removed, special characters decoded.

/// `%%c` → Ø, `%%d` → °, `%%p` → ±, `%%%` → %, `%%nnn` → character nnn; `%%u`/`%%o`/`%%k`
/// (underline/overline/strike toggles) are dropped.
/// Also decodes `\U+XXXX` (pre-2007 files write non-ASCII text this way).
pub fn text_plain(s: &str) -> String {
    percent_codes(&decode_unicode(s))
}

/// `\U+XXXX` / `\u+XXXX` → the character; anything else is kept. For TEXT, ATTRIB, layer and
/// block names (MTEXT goes through [`mtext_plain`]).
pub fn decode_unicode(s: &str) -> String {
    if !s.contains("\\U+") && !s.contains("\\u+") {
        return s.to_string();
    }
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\\'
            && matches!(c.get(i + 1), Some(&'U') | Some(&'u'))
            && c.get(i + 2) == Some(&'+')
        {
            let hex: String = c[i + 3..].iter().take(4).collect();
            if hex.len() == 4 && hex.chars().all(|x| x.is_ascii_hexdigit()) {
                let mut v = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFF_FFFF);
                let mut used = 7;
                if (0xD800..0xDC00).contains(&v) && c.get(i + 7) == Some(&'\\') {
                    // UTF-16 pair written for chars above U+FFFF
                    let t: String = c[i + 7..].iter().take(7).collect();
                    let lo = t
                        .strip_prefix("\\U+")
                        .or_else(|| t.strip_prefix("\\u+"))
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .filter(|l| (0xDC00..0xE000).contains(l));
                    if let Some(l) = lo {
                        v = 0x10000 + ((v - 0xD800) << 10) + (l - 0xDC00);
                        used = 14;
                    }
                }
                if let Some(u) = char::from_u32(v) {
                    out.push(u);
                    i += used;
                    continue;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn percent_codes(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '%' && c.get(i + 1) == Some(&'%') && i + 2 < c.len() {
            let k = c[i + 2];
            let mapped = match k.to_ascii_lowercase() {
                'c' => Some('\u{00D8}'),
                'd' => Some('\u{00B0}'),
                'p' => Some('\u{00B1}'),
                '%' => Some('%'),
                'u' | 'o' | 'k' => None,
                d if d.is_ascii_digit() => {
                    let digits: String = c[i + 2..]
                        .iter()
                        .take(3)
                        .take_while(|x| x.is_ascii_digit())
                        .collect();
                    let n = digits.len();
                    if let Some(ch) = digits.parse::<u32>().ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                    i += 2 + n;
                    continue;
                }
                _ => {
                    out.push('%');
                    i += 1;
                    continue;
                }
            };
            out.extend(mapped);
            i += 3;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// MTEXT without formatting: `\P` and `\X` give a space, `\~` a space, `\U+XXXX` the character,
/// `\S a^b;` "a/b"; codes with an argument (`\f..;`, `\H..;`, `\C..;` …) are dropped up to `;`,
/// toggles (`\L`, `\o`, `\K` …) are dropped alone; braces are removed. `%%` codes are decoded too.
pub fn mtext_plain(s: &str) -> String {
    mtext_with(s, " ")
}

/// Like [`mtext_plain`] but paragraph breaks (`\P`, `\X`) stay line breaks.
pub fn mtext_lines(s: &str) -> String {
    mtext_with(s, "\n")
}

fn mtext_with(s: &str, brk: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' => {
                let Some(&k) = c.get(i + 1) else {
                    break;
                };
                i += 2;
                match k {
                    'P' | 'X' => out.push_str(brk),
                    '~' => out.push(' '),
                    'p' if !arg_follows(&c, i) => out.push_str(brk),
                    '\\' | '{' | '}' => out.push(k),
                    'U' | 'u' if c.get(i) == Some(&'+') => {
                        let hex: String = c[i + 1..].iter().take(4).collect();
                        match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            Some(u) if hex.len() == 4 => {
                                out.push(u);
                                i += 5;
                            }
                            _ => out.push_str("\\U"),
                        }
                    }
                    // \M+nXXXX: double-byte code in a CJK page; shown as a placeholder.
                    'M' | 'm' if c.get(i) == Some(&'+') => {
                        out.push('?');
                        i = (i + 6).min(c.len());
                    }
                    'S' => {
                        let mut j = i;
                        while j < c.len() && c[j] != ';' {
                            j += 1;
                        }
                        let body: String = c[i..j].iter().collect();
                        let body = body.replace(['^', '#'], "/");
                        out.push_str(body.trim());
                        i = (j + 1).min(c.len());
                    }
                    'L' | 'l' | 'O' | 'o' | 'K' | 'k' | 'N' => {}
                    _ => {
                        // \f, \F, \H, \W, \Q, \T, \A, \C, \c, \p…: argument up to ';'
                        while i < c.len() && c[i] != ';' {
                            i += 1;
                        }
                        i = (i + 1).min(c.len());
                    }
                }
            }
            '{' | '}' => i += 1,
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    percent_codes(&out)
}

/// `\p` is a paragraph code (`\pxi..;`) when an argument letter follows, a break otherwise.
fn arg_follows(c: &[char], i: usize) -> bool {
    c.get(i)
        .is_some_and(|x| x.is_ascii_alphabetic() || *x == '-')
        && c[i..].iter().take(32).any(|x| *x == ';')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ru_fixtures::ru;

    #[test]
    fn percent_codes() {
        assert_eq!(text_plain("%%c200 %%d %%p5 100%%%"), "Ø200 ° ±5 100%");
        assert_eq!(text_plain(&format!("%%u{}%%u", ru("grid_a"))), ru("grid_a"));
        assert_eq!(text_plain("%%065B"), "AB");
        assert_eq!(text_plain("50%"), "50%");
    }

    #[test]
    fn text_unicode_escapes() {
        assert_eq!(
            text_plain("\\U+041A\\U+0443\\U+0445\\U+043D\\U+044F 12"),
            format!("{} 12", ru("kitchen"))
        );
        assert_eq!(decode_unicode("\\u+0421\\U+0422-1"), ru("st_1"));
        assert_eq!(decode_unicode("a\\U+00"), "a\\U+00");
        assert_eq!(decode_unicode("x\\U+ZZZZ"), "x\\U+ZZZZ");
        assert_eq!(text_plain("%%c\\U+0414"), format!("Ø{}", ru("cyr_de")));
    }

    #[test]
    fn surrogate_pair_decodes_both_cases() {
        assert_eq!(decode_unicode("\\U+D83D\\U+DE00!"), "\u{1F600}!");
        assert_eq!(decode_unicode("\\U+D83D\\u+DE00"), "\u{1F600}");
        assert_eq!(crate::dxf::writer::escape_non_ascii("\u{1F600}"), "\\U+D83D\\U+DE00");
    }

    #[test]
    fn mtext_unicode_does_not_eat_tail() {
        assert_eq!(
            mtext_plain(&format!("\\U+041A\\U+0443{} 12", ru("xnya"))),
            format!("{} 12", ru("kitchen"))
        );
        assert_eq!(
            mtext_plain(&format!("{{\\fArial|b0;{}}}\\PS=18", ru("residential"))),
            format!("{} S=18", ru("residential"))
        );
        assert_eq!(
            mtext_plain(&format!("\\L{}\\l {}", ru("sub_word"), ru("no_word"))),
            format!("{} {}", ru("sub_word"), ru("no_word"))
        );
        assert_eq!(
            mtext_plain(&format!("\\S1^2; {}", ru("meter_letter"))),
            format!("1/2 {}", ru("meter_letter"))
        );
        assert_eq!(mtext_plain("\\H2.5x;Big\\~x"), "Big x");
        assert_eq!(
            mtext_plain(&format!("\\pxi-3,l3;{}", ru("item_word"))),
            ru("item_word")
        );
        assert_eq!(mtext_plain("%%c50"), "Ø50");
        assert_eq!(mtext_plain("a\\\\b\\{c\\}"), "a\\b{c}");
    }
}

#[cfg(test)]
mod lines_tests {
    use super::*;

    #[test]
    fn mtext_lines_keeps_breaks() {
        assert_eq!(mtext_lines("{\\fArial|b0;A}\\PB\\~C"), "A\nB C");
        assert_eq!(mtext_plain("A\\PB"), "A B");
    }
}
