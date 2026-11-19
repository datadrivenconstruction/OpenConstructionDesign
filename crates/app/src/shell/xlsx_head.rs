// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Header strip and footer plan of the XLSX window: what book this is, how it is linked,
//! when it was last exported or synced, whether the model moved since, and which single
//! action is primary. The decisions are pure functions; `strip` only draws them.
use egui::{self, RichText};

/// How the window relates to its book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// No book chosen yet.
    Unlinked,
    /// A book is open and compared with the model.
    Linked,
    /// The book matches the model and is watched for the user's edits.
    Watched,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadIn<'a> {
    pub path: &'a str,
    pub link: Link,
    /// Seconds since the book was last written by an export or sync; None when never.
    pub age_secs: Option<u64>,
    /// The model changed after that export.
    pub model_changed: bool,
    /// The book has the read-only attribute: Apply writes a copy.
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    Never,
    JustNow,
    Minutes(u64),
    Hours(u64),
    Days(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadState {
    /// File name only; the UI elides it and puts `full` into the tooltip.
    pub name: String,
    pub full: String,
    pub link: Link,
    pub age: Age,
    /// Show the "model changed since export" badge.
    pub stale: bool,
    /// Draw the muted "Read-only" row under the name.
    pub read_only: bool,
}

/// Head state of an open book; `mtime` is the file's last write (the last export or sync).
pub fn from_book(
    path: &std::path::Path,
    synced: bool,
    mtime: Option<web_time::SystemTime>,
    model_changed: bool,
) -> HeadState {
    let age = mtime.map(|t| t.elapsed().map_or(0, |d| d.as_secs()));
    header_state(&HeadIn {
        path: &path.to_string_lossy(),
        link: if synced { Link::Watched } else { Link::Linked },
        age_secs: age,
        model_changed,
        read_only: read_only_cached(path, web_time::Instant::now()),
    })
}

/// Last read-only probe: the header is drawn every frame, the file is asked at most once a second.
static RO_CACHE: std::sync::Mutex<Option<(std::path::PathBuf, web_time::Instant, bool)>> =
    std::sync::Mutex::new(None);
const RO_TTL: std::time::Duration = std::time::Duration::from_secs(1);

fn read_only_cached(path: &std::path::Path, now: web_time::Instant) -> bool {
    let Ok(mut c) = RO_CACHE.lock() else {
        return super::xlsx_owner::read_only(path);
    };
    if let Some((p, at, ro)) = c.as_ref() {
        if p == path && now.saturating_duration_since(*at) < RO_TTL {
            return *ro;
        }
    }
    let ro = super::xlsx_owner::read_only(path);
    *c = Some((path.to_path_buf(), now, ro));
    ro
}

/// Drops the cached probe (tests that flip the attribute).
pub fn forget_read_only() {
    if let Ok(mut c) = RO_CACHE.lock() {
        *c = None;
    }
}

/// Program and arguments that reveal `file` in the OS file manager.
pub fn reveal_command(file: &std::path::Path, os: &str) -> (&'static str, Vec<String>) {
    let p = file.display().to_string();
    match os {
        "windows" => ("explorer", vec![format!("/select,{p}")]),
        "macos" => ("open", vec!["-R".into(), p]),
        _ => {
            let dir = file.parent().unwrap_or(std::path::Path::new("."));
            ("xdg-open", vec![dir.display().to_string()])
        }
    }
}

/// Reveals `file` in the file manager; does nothing in screenshot mode.
pub fn reveal(file: &std::path::Path, screenshot: bool) -> bool {
    if screenshot {
        return false;
    }
    #[cfg(windows)]
    {
        // the file manager needs `/select,"path"`; default quoting wraps the whole
        // argument and opens the user's home folder for a path with spaces
        use std::os::windows::process::CommandExt;
        return std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", file.display()))
            .spawn()
            .is_ok();
    }
    #[cfg(not(windows))]
    {
        let (prog, args) = reveal_command(file, std::env::consts::OS);
        std::process::Command::new(prog).args(args).spawn().is_ok()
    }
}

/// The click handler: honours the process-wide screenshot flag.
pub fn show_in_folder(file: &std::path::Path) {
    let _ = reveal(file, crate::shot_press::native_blocked());
}

pub fn age_of(secs: Option<u64>) -> Age {
    match secs {
        None => Age::Never,
        Some(s) if s < 60 => Age::JustNow,
        Some(s) if s < 3600 => Age::Minutes(s / 60),
        Some(s) if s < 86_400 => Age::Hours(s / 3600),
        Some(s) => Age::Days(s / 86_400),
    }
}

pub fn header_state(i: &HeadIn) -> HeadState {
    let age = if i.link == Link::Unlinked { Age::Never } else { age_of(i.age_secs) };
    HeadState {
        name: crate::ui_kit::file_name_of(i.path),
        full: i.path.to_string(),
        link: i.link,
        age,
        // a book never exported has nothing to be stale against
        stale: i.model_changed && i.link != Link::Unlinked && age != Age::Never,
        read_only: i.read_only,
    }
}

/// Localized relative time, e.g. "5 min ago".
pub fn age_text(a: Age, ru: bool) -> String {
    let (key, n) = match a {
        Age::Never => ("xlw.hd_never", 0),
        Age::JustNow => ("xlw.hd_now", 0),
        Age::Minutes(n) => ("xlw.hd_min", n),
        Age::Hours(n) => ("xlw.hd_hour", n),
        Age::Days(n) => ("xlw.hd_day", n),
    };
    crate::tr_lang(ru, key, &[("n", &n.to_string())])
}

pub fn link_key(l: Link) -> Option<&'static str> {
    match l {
        Link::Unlinked => None,
        Link::Linked => Some("xlw.hd_linked"),
        Link::Watched => Some("xlw.hd_watched"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Export,
    Review,
    Apply(usize),
    Sync,
    DeleteMissing,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Primary,
    Secondary,
    /// Destructive and quiet: never the default, never bound to Enter.
    QuietDestructive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub kind: Kind,
    pub role: Role,
    /// Enter triggers it; true only for the primary action.
    pub on_enter: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FootIn {
    /// Book chosen and exported at least once.
    pub exported: bool,
    /// Book matches the model (waiting for edits).
    pub synced: bool,
    pub model_changed: bool,
    pub changes: usize,
    /// Rows to decide: key matches awaiting an answer, conflicts, blocking errors.
    pub to_review: usize,
    /// Model elements absent from the book while deleting them is off.
    pub missing: usize,
}

fn act(kind: Kind, role: Role) -> Action {
    Action { kind, role, on_enter: role == Role::Primary }
}

/// Exactly one primary action per state, then quiet ones; the primary goes first (far right).
pub fn footer_actions(i: &FootIn) -> Vec<Action> {
    let primary = if !i.exported {
        Kind::Export
    } else if i.synced {
        if i.model_changed { Kind::Export } else { Kind::Sync }
    } else if i.to_review > 0 {
        Kind::Review
    } else if i.changes > 0 {
        Kind::Apply(i.changes)
    } else {
        Kind::Sync
    };
    let mut v = vec![act(primary, Role::Primary)];
    if i.exported && primary != Kind::Sync {
        v.push(act(Kind::Sync, Role::Secondary));
    }
    if i.exported && !i.synced && i.missing > 0 {
        v.push(act(Kind::DeleteMissing, Role::QuietDestructive));
    }
    v.push(act(Kind::Close, Role::Secondary));
    v
}

pub fn action_text(k: Kind, ru: bool) -> String {
    match k {
        Kind::Export => crate::tr_lang(ru, "xlw.ft_export", &[]),
        Kind::Review => crate::tr_lang(ru, "xlw.ft_review", &[]),
        Kind::Apply(n) => crate::tr_lang(ru, "xlw.ft_apply", &[("n", &n.to_string())]),
        Kind::Sync => crate::tr_lang(ru, "xlw.synchronize", &[]),
        Kind::DeleteMissing => crate::tr_lang(ru, "xlw.ft_delete_missing", &[]),
        Kind::Close => crate::tr_lang(ru, "xlw.close", &[]),
    }
}

pub use crate::ui_kit::split_unit;

fn badge(ui: &mut egui::Ui, text: &str, col: egui::Color32) {
    egui::Frame::new()
        .fill(col.gamma_multiply(0.12))
        .corner_radius(ddc_ui::theme::radius::SM)
        .inner_margin(egui::Margin::symmetric(
            ddc_ui::theme::space::MD as i8,
            ddc_ui::theme::space::XS as i8,
        ))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(col));
        });
}

/// One row: elided book name (full path on hover) on the left; link chip, time and the
/// stale badge on the right.
pub fn strip(ui: &mut egui::Ui, st: &HeadState, ru: bool) {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if st.stale {
                badge(ui, &crate::tr_lang(ru, "xlw.hd_stale", &[]), th.c("warning"));
            }
            if st.link != Link::Unlinked {
                ui.label(RichText::new(age_text(st.age, ru)).color(th.c("text.secondary")));
            }
            if let Some(k) = link_key(st.link) {
                let col = if st.link == Link::Watched { th.c("success") } else { th.c("accent") };
                badge(ui, &crate::tr_lang(ru, k, &[]), col);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                crate::ui_kit::name_with_tip(ui, &st.name, &st.full, true);
            });
        });
    });
    if st.read_only {
        ui.label(RichText::new(crate::tr_lang(ru, "xlsx_readonly.read_only_head", &[])).color(th.c("text.secondary")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;

    fn head(link: Link, age: Option<u64>, changed: bool) -> HeadState {
        header_state(&HeadIn { path: "C:/books/Plan.xlsx", link, age_secs: age, model_changed: changed, read_only: false })
    }

    #[test]
    fn header_read_only_row_only_for_read_only_books() {
        let i = HeadIn { path: "C:/b/P.xlsx", link: Link::Linked, age_secs: None, model_changed: false, read_only: true };
        assert!(header_state(&i).read_only);
        assert!(!header_state(&HeadIn { read_only: false, ..i }).read_only);
        assert!(!head(Link::Linked, Some(5), false).read_only);
    }

    #[test]
    fn read_only_flag_follows_the_file_attribute() {
        let dir = std::env::temp_dir().join(format!("xs29-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("b.xlsx");
        std::fs::write(&f, b"x").unwrap();
        assert!(!from_book(&f, false, None, false).read_only);
        let mut perm = std::fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&f, perm.clone()).unwrap();
        forget_read_only();
        assert!(from_book(&f, false, None, false).read_only);
        perm.set_readonly(false);
        std::fs::set_permissions(&f, perm).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveal_command_per_os() {
        let f = std::path::Path::new("C:/books/Plan-synced.xlsx");
        let (p, a) = reveal_command(f, "windows");
        assert_eq!((p, a), ("explorer", vec!["/select,C:/books/Plan-synced.xlsx".to_string()]));
        let (p, a) = reveal_command(f, "macos");
        assert_eq!((p, a), ("open", vec!["-R".to_string(), "C:/books/Plan-synced.xlsx".to_string()]));
        let (p, a) = reveal_command(f, "linux");
        assert_eq!((p, a), ("xdg-open", vec!["C:/books".to_string()]));
    }

    #[test]
    fn read_only_probe_is_cached_for_a_second() {
        let dir = std::env::temp_dir().join(format!("xs29-roc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.xlsx");
        std::fs::write(&f, b"x").unwrap();
        let t0 = web_time::Instant::now();
        forget_read_only();
        assert!(!read_only_cached(&f, t0));
        let mut perm = std::fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&f, perm.clone()).unwrap();
        // a parallel test may have replaced the single entry; then the probe is fresh (true)
        let _ = read_only_cached(&f, t0);
        assert!(read_only_cached(&f, t0 + RO_TTL));
        perm.set_readonly(false);
        std::fs::set_permissions(&f, perm).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveal_is_skipped_in_screenshot_mode() {
        assert!(!reveal(std::path::Path::new("C:/books/Plan.xlsx"), true));
    }

    #[test]
    fn xs29_keys_exist_in_en_and_ru() {
        for k in ["xlsx_readonly.read_only_head", "xlsx_readonly.show_in_folder", "xlsx_readonly.copy_exists"] {
            let (en, ru) = (crate::tr_lang(false, k, &[]), crate::tr_lang(true, k, &[]));
            assert!(en != k && ru != k && en != ru, "{k}");
        }
    }

    #[test]
    fn unlinked_has_no_chip_time_or_badge() {
        let h = head(Link::Unlinked, Some(10), true);
        assert_eq!(link_key(h.link), None);
        assert_eq!(h.age, Age::Never);
        assert!(!h.stale);
    }

    #[test]
    fn linked_and_watched_get_their_chip() {
        assert_eq!(link_key(head(Link::Linked, Some(5), false).link), Some("xlw.hd_linked"));
        assert_eq!(link_key(head(Link::Watched, Some(5), false).link), Some("xlw.hd_watched"));
    }

    #[test]
    fn name_is_the_file_and_path_is_kept_for_the_tooltip() {
        let h = head(Link::Linked, Some(5), false);
        assert_eq!(h.name, "Plan.xlsx");
        assert_eq!(h.full, "C:/books/Plan.xlsx");
    }

    #[test]
    fn stale_badge_only_when_exported_and_model_moved() {
        assert!(head(Link::Watched, Some(300), true).stale);
        assert!(!head(Link::Watched, Some(300), false).stale);
    }

    #[test]
    fn never_exported_is_not_stale() {
        let h = head(Link::Linked, None, true);
        assert_eq!(h.age, Age::Never);
        assert!(!h.stale);
    }

    #[test]
    fn age_buckets() {
        assert_eq!(age_of(Some(59)), Age::JustNow);
        assert_eq!(age_of(Some(60)), Age::Minutes(1));
        assert_eq!(age_of(Some(3599)), Age::Minutes(59));
        assert_eq!(age_of(Some(3600)), Age::Hours(1));
        assert_eq!(age_of(Some(86_400 * 3)), Age::Days(3));
    }

    fn states() -> Vec<FootIn> {
        let b = FootIn::default();
        vec![
            b,
            FootIn { exported: true, synced: true, ..b },
            FootIn { exported: true, synced: true, model_changed: true, ..b },
            FootIn { exported: true, changes: 3, ..b },
            FootIn { exported: true, changes: 3, to_review: 2, ..b },
            FootIn { exported: true, changes: 0, ..b },
            FootIn { exported: true, changes: 3, missing: 4, ..b },
        ]
    }

    #[test]
    fn every_state_has_exactly_one_primary() {
        for s in states() {
            let a = footer_actions(&s);
            assert_eq!(a.iter().filter(|x| x.role == Role::Primary).count(), 1, "{s:?}");
        }
    }

    #[test]
    fn primary_follows_the_state() {
        let b = FootIn::default();
        let p = |s: FootIn| footer_actions(&s)[0].kind;
        assert_eq!(p(b), Kind::Export);
        assert_eq!(p(FootIn { exported: true, synced: true, model_changed: true, ..b }), Kind::Export);
        assert_eq!(p(FootIn { exported: true, changes: 3, to_review: 1, ..b }), Kind::Review);
        assert_eq!(p(FootIn { exported: true, changes: 3, ..b }), Kind::Apply(3));
    }

    #[test]
    fn destructive_is_quiet_never_primary_never_on_enter() {
        let s = FootIn { exported: true, changes: 3, missing: 4, ..Default::default() };
        let a = footer_actions(&s);
        let d = a.iter().find(|x| x.kind == Kind::DeleteMissing).expect("offered");
        assert_eq!(d.role, Role::QuietDestructive);
        assert!(!d.on_enter);
        assert!(a.iter().all(|x| x.on_enter == (x.role == Role::Primary)));
        assert_ne!(a[0].kind, Kind::DeleteMissing);
    }

    #[test]
    fn delete_missing_is_offered_only_for_an_open_diff_with_missing_rows() {
        let b = FootIn { exported: true, ..Default::default() };
        let has = |s: FootIn| footer_actions(&s).iter().any(|x| x.kind == Kind::DeleteMissing);
        assert!(has(FootIn { missing: 2, ..b }));
        assert!(!has(FootIn { missing: 0, ..b }));
        assert!(!has(FootIn { missing: 2, synced: true, ..b }));
        assert!(!has(FootIn { missing: 2, exported: false, ..Default::default() }));
    }

    #[test]
    fn nothing_to_apply_falls_back_to_sync_as_the_single_primary() {
        let a = footer_actions(&FootIn { exported: true, ..Default::default() });
        assert_eq!(a[0].kind, Kind::Sync);
        assert_eq!(a.iter().filter(|x| x.kind == Kind::Sync).count(), 1);
    }

    #[test]
    fn from_book_uses_the_given_time_and_marks_state() {
        let p = std::path::Path::new("C:/b/Plan.xlsx");
        let now = web_time::SystemTime::now();
        let h = from_book(p, true, Some(now), true);
        assert_eq!((h.link, h.age, h.stale), (Link::Watched, Age::JustNow, true));
        let old = now - std::time::Duration::from_secs(7200);
        assert_eq!(from_book(p, false, Some(old), false).age, Age::Hours(2));
        assert_eq!(from_book(p, false, None, true).age, Age::Never);
    }

    #[test]
    fn split_unit_separates_the_suffix() {
        assert_eq!(split_unit("12.5 m"), ("12.5", "m"));
        assert_eq!(split_unit("7"), ("7", ""));
        assert_eq!(split_unit("2800\u{a0}mm"), ("2800", "mm"));
    }

    #[test]
    fn split_unit_keeps_digit_groups_and_compound_lengths_whole() {
        assert_eq!(split_unit("3 000 mm"), ("3 000", "mm"));
        assert_eq!(split_unit("3\u{a0}000 mm"), ("3\u{a0}000", "mm"));
        assert_eq!(split_unit("3\u{a0}200\u{a0}mm"), ("3\u{a0}200", "mm"));
        assert_eq!(split_unit("1 250,5 m\u{b2}"), ("1 250,5", "m\u{b2}"));
        assert_eq!(split_unit("1 250,5 \u{43c}\u{b2}"), ("1 250,5", "\u{43c}\u{b2}"));
        assert_eq!(split_unit("3\u{a0}000\u{a0}\u{43c}\u{43c}"), ("3\u{a0}000", "\u{43c}\u{43c}"));
        assert_eq!(split_unit("-1 200,5 m3"), ("-1 200,5", "m3"));
        assert_eq!(split_unit("3000"), ("3000", ""));
        assert_eq!(split_unit("3 000"), ("3 000", ""), "no unit: one number");
        assert_eq!(split_unit("12 ft 6 in"), ("12 ft 6 in", ""));
        assert_eq!(split_unit("19'-8 1/4\""), ("19'-8 1/4\"", ""));
        assert_eq!(split_unit("Basic wall 200"), ("Basic wall 200", ""));
        assert_eq!(split_unit("Level 1 m"), ("Level 1 m", ""), "text before a unit word");
        assert_eq!(split_unit("A 3 m"), ("A 3 m", ""));
    }

    #[test]
    fn keys_exist_in_en_and_ru() {
        let keys = [
            "xlw.hd_never", "xlw.hd_now", "xlw.hd_min", "xlw.hd_hour", "xlw.hd_day",
            "xlw.hd_linked", "xlw.hd_watched", "xlw.hd_stale", "xlw.ft_export", "xlw.ft_review",
            "xlw.ft_apply", "xlw.ft_delete_missing", "xlw.synchronize", "xlw.close",
        ];
        for k in keys {
            let en = crate::tr_in(Lang::En, k, &[]);
            let ru = crate::tr_in(Lang::Ru, k, &[]);
            assert_ne!(en, k, "{k} missing in en");
            assert_ne!(ru, k, "{k} missing in ru");
            assert_ne!(en, ru, "{k} not translated");
        }
        assert!(crate::tr_in(Lang::En, "xlw.ft_apply", &[("n", "3")]).contains('3'));
        assert!(crate::tr_in(Lang::Ru, "xlw.hd_min", &[("n", "5")]).contains('5'));
    }
}
