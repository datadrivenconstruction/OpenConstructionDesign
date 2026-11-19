//! The size of the next message against what the connection can take. The estimate is the one the
//! conversation uses (`estimate_text_tokens`), the block limit is the one it trims history at
//! (`context_budget`), so the composer never promises more than a turn can carry. The input is never
//! cut on its own: the way out is a click.

use super::ctx_chips::{self, Body, Chip, Ev, Kind};
use super::kit::Button;
use super::{Cx, PanelUi};
use ddc_ai::{char_quarter_tokens, context_budget, estimate_text_tokens};
use ddc_ui::theme::Role;
use ddc_ui::{Catalog, Theme};
use egui::{RichText, Ui};

/// Share of the context (percent) where the counter appears and where it turns to a warning.
const COUNT_PCT: u64 = 10;
const WARN_PCT: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Quiet,
    Count,
    Warn,
    Block,
}

/// What the next turn weighs and what it is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub tokens: u64,
    pub context: u32,
    pub level: Level,
}

/// One click that brings the message under the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    Drop(Kind),
    Trim,
}

/// Quiet below 10 % of the context, a counter from there, a warning from 50 %, blocked above the
/// conversation's own limit. An unknown limit (0) never says anything and never blocks.
pub fn input_budget(tokens: u64, context: u32) -> Level {
    if context == 0 {
        return Level::Quiet;
    }
    let ctx = u64::from(context);
    if tokens > context_budget(context) {
        Level::Block
    } else if tokens * 100 >= ctx * WARN_PCT {
        Level::Warn
    } else if tokens * 100 >= ctx * COUNT_PCT {
        Level::Count
    } else {
        Level::Quiet
    }
}

/// What a context chip adds to the note of the turn; a chip that is left out adds nothing.
pub fn chip_tokens(c: &Chip) -> u64 {
    if !c.on {
        return 0;
    }
    let note = match &c.body {
        Body::View3d(d) => format!("view={}", if *d { "3d" } else { "plan" }),
        Body::Level(_) => "level_id=1000".to_string(),
        Body::Selection { total, .. } => format!("selected={total}"),
    };
    estimate_text_tokens(&note).max(1)
}

pub fn chips_tokens(chips: &[Chip]) -> u64 {
    chips.iter().map(chip_tokens).sum()
}

pub fn measure(input: &str, chips: &[Chip], context: u32) -> Budget {
    let tokens = estimate_text_tokens(input) + chips_tokens(chips);
    Budget {
        tokens,
        context,
        level: input_budget(tokens, context),
    }
}

/// Send and Enter need text, an idle worker and a message that fits.
pub fn can_send(input: &str, busy: bool, level: Level) -> bool {
    !input.trim().is_empty() && !busy && level != Level::Block
}

/// For a blocked message: leave the largest chip out when that alone makes it fit, else trim.
pub fn way_out(input: &str, chips: &[Chip], context: u32) -> Option<Way> {
    if measure(input, chips, context).level != Level::Block {
        return None;
    }
    let big = chips
        .iter()
        .enumerate()
        .filter(|(_, c)| c.on)
        .max_by_key(|(i, c)| (chip_tokens(c), std::cmp::Reverse(*i)))
        .map(|(_, c)| c);
    if let Some(c) = big {
        let rest: Vec<Chip> = chips
            .iter()
            .map(|x| Chip {
                on: x.on && x.kind != c.kind,
                ..x.clone()
            })
            .collect();
        if measure(input, &rest, context).level != Level::Block {
            return Some(Way::Drop(c.kind));
        }
    }
    Some(Way::Trim)
}

/// The longest start of `input` that fits next to the chips, cut on a character boundary.
pub fn trim_to_fit(input: &str, chips: &[Chip], context: u32) -> String {
    if context == 0 {
        return input.to_string();
    }
    let room = context_budget(context).saturating_sub(chips_tokens(chips));
    // quarters / 4 <= room  <=>  quarters <= room * 4 + 3, counted per character like the estimate
    let max = room.saturating_mul(4).saturating_add(3);
    let mut quarters = 0u64;
    for (i, c) in input.char_indices() {
        quarters += char_quarter_tokens(c);
        if quarters > max {
            return input[..i].trim_end().to_string();
        }
    }
    input.to_string()
}

/// `128000` as `128,000` (English) or with a no-break space (Russian): the separator is a string.
pub fn group(n: u64, sep: &str) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

fn compact(n: u64, sep: &str) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else {
        group(n, sep)
    }
}

pub fn counter(cat: &Catalog, b: &Budget) -> String {
    let sep = cat.t("ai.size.sep");
    cat.tf(
        "ai.size.count",
        &[
            ("n", &compact(b.tokens, &sep)),
            ("max", &compact(u64::from(b.context), &sep)),
        ],
    )
}

pub fn reason(cat: &Catalog, b: &Budget) -> String {
    let sep = cat.t("ai.size.sep");
    cat.tf(
        "ai.size.block",
        &[
            ("n", &group(b.tokens, &sep)),
            ("max", &group(context_budget(b.context), &sep)),
        ],
    )
}

fn drop_key(k: Kind) -> &'static str {
    match k {
        Kind::View => "ai.size.drop_view",
        Kind::Level => "ai.size.drop_level",
        Kind::Selection => "ai.size.drop_selection",
    }
}

pub fn context_of(st: &PanelUi) -> u32 {
    st.profiles.get(st.profile).map_or(0, |p| p.context_tokens)
}

/// The chips as the strip above draws them.
pub fn live_chips(st: &PanelUi, cx: &Cx<'_>) -> Vec<Chip> {
    let levels = cx.ed.levels();
    let level = levels
        .iter()
        .find(|l| l.0 == cx.ed.level)
        .map_or("", |l| l.1.as_str());
    ctx_chips::chips(
        cx.view3d,
        level,
        &ctx_chips::counts(&cx.ed.selection),
        st.ctx_off,
        st.use_selection,
    )
}

/// The counter line and, when blocked, the reason with its way out. Returns the click.
pub fn row(ui: &mut Ui, th: &Theme, cat: &Catalog, b: &Budget, way: Option<Way>) -> Option<Way> {
    let (color, text) = match b.level {
        Level::Quiet => return None,
        Level::Count => (th.c("text.secondary"), counter(cat, b)),
        Level::Warn => (th.c("warning"), counter(cat, b)),
        Level::Block => (th.c("error"), reason(cat, b)),
    };
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(text).font(th.font(Role::Caption)).color(color));
        if b.level != Level::Block {
            return;
        }
        if let Some(Way::Drop(k)) = way {
            if Button::secondary(&cat.t(drop_key(k))).small().show(ui, th).clicked() {
                clicked = Some(Way::Drop(k));
            }
        }
        if Button::secondary(&cat.t("ai.size.trim"))
            .small()
            .tooltip(&cat.t("ai.size.trim_tip"), "")
            .show(ui, th)
            .clicked()
        {
            clicked = Some(Way::Trim);
        }
    });
    clicked
}

/// Runs the chosen way out on the panel's state.
pub fn apply(way: Way, st: &mut PanelUi, chips: &[Chip], context: u32) {
    match way {
        Way::Drop(k) => ctx_chips::run(Ev::Remove(k), &mut st.ctx_off, &mut st.use_selection),
        Way::Trim => st.input = trim_to_fit(&st.input, chips, context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn chips() -> Vec<Chip> {
        ctx_chips::chips(
            false,
            "L1",
            &[("wall".to_string(), 12)],
            ctx_chips::Off::default(),
            true,
        )
    }

    fn text(tokens: usize) -> String {
        "a".repeat(tokens * 4)
    }

    #[test]
    fn estimate_is_monotonic_and_deterministic() {
        let mut last = 0;
        for n in 0..200 {
            let s = "ab".repeat(n);
            let a = measure(&s, &[], 1000).tokens;
            assert_eq!(a, measure(&s, &[], 1000).tokens);
            assert!(a >= last);
            last = a;
        }
        assert_eq!(measure("x", &[], 100).tokens, estimate_text_tokens("x"));
        let same = text(37);
        assert_eq!(measure(&same, &[], 0).tokens, estimate_text_tokens(&same));
    }

    #[test]
    fn context_chips_count_and_left_out_chips_do_not() {
        let on = chips();
        assert!(chips_tokens(&on) >= on.len() as u64);
        assert_eq!(
            measure("", &on, 1000).tokens,
            chips_tokens(&on),
            "an empty message still carries its context"
        );
        let off: Vec<Chip> = on.iter().map(|c| Chip { on: false, ..c.clone() }).collect();
        assert_eq!(chips_tokens(&off), 0);
        assert!(measure("hello world", &on, 1000).tokens > measure("hello world", &off, 1000).tokens);
    }

    #[test]
    fn thresholds_ok_warn_block() {
        // context 1000: counter from 100, warning from 500, blocked above 700
        assert_eq!(input_budget(99, 1000), Level::Quiet);
        assert_eq!(input_budget(100, 1000), Level::Count);
        assert_eq!(input_budget(499, 1000), Level::Count);
        assert_eq!(input_budget(500, 1000), Level::Warn);
        assert_eq!(input_budget(700, 1000), Level::Warn);
        assert_eq!(input_budget(701, 1000), Level::Block);
        assert_eq!(context_budget(1000), 700);
    }

    #[test]
    fn unknown_limit_never_blocks() {
        assert_eq!(input_budget(10_000_000, 0), Level::Quiet);
        let b = measure(&text(5000), &chips(), 0);
        assert!(can_send("hello", false, b.level));
        assert_eq!(way_out(&text(5000), &chips(), 0), None);
        assert_eq!(trim_to_fit("abc", &[], 0), "abc");
    }

    #[test]
    fn enter_on_block_sends_nothing() {
        assert!(can_send("hi", false, Level::Warn));
        assert!(!can_send("hi", false, Level::Block));
        assert!(!can_send("hi", true, Level::Quiet));
        assert!(!can_send("  ", false, Level::Quiet));
    }

    #[test]
    fn block_reason_is_localized_with_thousands() {
        let b = Budget {
            tokens: 2_000_000,
            context: 1_000_000,
            level: Level::Block,
        };
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "ai.size.block",
                "ai.size.count",
                "ai.size.sep",
                "ai.size.trim",
                "ai.size.trim_tip",
                "ai.size.drop_view",
                "ai.size.drop_level",
                "ai.size.drop_selection",
            ] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
            let sep = cat.t("ai.size.sep");
            let r = reason(&cat, &b);
            assert!(r.contains(&format!("2{sep}000{sep}000")), "{r}");
            assert!(r.contains(&format!("700{sep}000")), "{r}");
            assert!(!r.contains('{'), "{r}");
        }
        let en = reason(&Catalog::new(Lang::En), &b);
        let ru = reason(&Catalog::new(Lang::Ru), &b);
        assert_ne!(en, ru);
        assert!(en.contains("2,000,000"));
        assert!(!ru.contains("2,000,000"));
        // the 70 % limit is the history-trim budget: the rest is the reply's room, and it says so
        assert!(en.contains("room for the reply"), "{en}");
        assert_eq!(context_budget(b.context), 700_000);
    }

    #[test]
    fn counter_is_compact() {
        let cat = Catalog::new(Lang::En);
        let b = Budget {
            tokens: 12_345,
            context: 128_000,
            level: Level::Count,
        };
        assert_eq!(counter(&cat, &b), cat.tf("ai.size.count", &[("n", "12k"), ("max", "128k")]));
        assert_eq!(group(0, ","), "0");
        assert_eq!(group(999, ","), "999");
        assert_eq!(group(1000, ","), "1,000");
        assert_eq!(group(1_234_567, " "), "1 234 567");
    }

    #[test]
    fn dropping_the_largest_chip_fits_when_it_can() {
        let c = chips();
        let ctx = 20; // limit 14
        let big = c.iter().filter(|x| x.on).max_by_key(|x| chip_tokens(x)).unwrap().kind;
        let room = 14 - chips_tokens(&c);
        let input = text(room as usize + 1); // one token over
        assert_eq!(measure(&input, &c, ctx).level, Level::Block);
        let way = way_out(&input, &c, ctx);
        assert_eq!(way, Some(Way::Drop(big)));
        let mut after = c.clone();
        for x in &mut after {
            if x.kind == big {
                x.on = false;
            }
        }
        assert_ne!(measure(&input, &after, ctx).level, Level::Block);
    }

    #[test]
    fn trim_brings_a_huge_message_under_the_limit() {
        let c = chips();
        let input = text(300_000);
        assert_eq!(way_out(&input, &c, 128_000), Some(Way::Trim));
        let cut = trim_to_fit(&input, &c, 128_000);
        let b = measure(&cut, &c, 128_000);
        assert_ne!(b.level, Level::Block);
        assert!(b.tokens > context_budget(128_000) - 4, "keeps as much as fits");
        assert!(input.starts_with(&cut));
        // a message that fits is left alone
        assert_eq!(trim_to_fit("short", &c, 128_000), "short");
        // multi-byte text is cut on a boundary
        let ru = "\u{44f}".repeat(1000);
        let cut = trim_to_fit(&ru, &[], 100);
        assert!(measure(&cut, &[], 100).level != Level::Block);
        assert!(ru.starts_with(&cut));
        // a token a character for CJK, Thai and Devanagari: the cut still fits
        for dense in ["\u{5efa}", "\u{e01}", "\u{915}", "\u{1f3e0}"] {
            let s = dense.repeat(1000);
            let cut = trim_to_fit(&s, &[], 100);
            assert!(!cut.is_empty() && s.starts_with(&cut));
            assert!(measure(&cut, &[], 100).level != Level::Block, "{dense}");
        }
        assert_eq!(way_out("hi", &c, 128_000), None);
    }
}
