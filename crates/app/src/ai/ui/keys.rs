// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Keys and tokens in the connection form. A credential pasted into the headers box is caught
//! while it is typed and moved to the key field with one click; key fields are masked by
//! default with show and copy; the token limits are locale number fields with presets.

use super::kit::{self, Button};
use super::logic;
use ddc_ai::AuthKind;
use ddc_ui::widgets::{NumberField, NumberFormat};
use ddc_ui::{Catalog, Theme};
use egui::{self, TextEdit, Ui};

// one rule for the form and the file on disk
use ddc_ai::secret::split_scheme;

const MASK_DOTS: usize = 8;

pub const TOKEN_PRESETS: [u32; 5] = [8_192, 32_768, 131_072, 200_000, 1_000_000];

/// The first header line that carries a credential, by its name or by its value, as the line
/// index and the header name.
pub fn header_issue(text: &str) -> Option<(usize, String)> {
    text.lines()
        .enumerate()
        .find_map(|(i, l)| line_key_name(l).map(|n| (i, n.to_string())))
}

/// The header name of a line that carries a credential.
fn line_key_name(l: &str) -> Option<&str> {
    let (name, value) = l.split_once(':')?;
    let (name, value) = (name.trim(), value.trim());
    // the same rule as the file and the wire (an id-like name keeps an id value)
    let secret = !value.is_empty() && ddc_ai::profile::header_is_secret(name, value);
    (!name.is_empty() && secret).then_some(name)
}

/// The headers text as the box paints it while masked: the value of every credential line
/// becomes dots, char for char (the cursor maps 1:1), line breaks and other lines stay.
pub fn mask_header_values(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for l in text.split_inclusive('\n') {
        let body = l.trim_end_matches(['\n', '\r']);
        match body.split_once(':').filter(|_| line_key_name(body).is_some()) {
            Some((name, value)) => {
                out.push_str(name);
                out.push(':');
                out.extend(value.chars().map(|c| if c == ' ' { ' ' } else { '\u{2022}' }));
                out.push_str(&l[body.len()..]);
            }
            None => out.push_str(l),
        }
    }
    out
}

/// The headers box: multi-line, with only the credential values masked while `mask` is on
/// (a password box would turn every line, breaks included, into one run of dots).
pub fn headers_edit(ui: &mut Ui, buf: &mut String, id: egui::Id, hint: &str, mask: bool) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui
        .visuals()
        .override_text_color
        .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
    let mut lay = move |ui: &Ui, t: &dyn egui::TextBuffer, wrap: f32| {
        let s = egui::TextBuffer::as_str(t);
        let shown = if mask { mask_header_values(s) } else { s.to_string() };
        let job = egui::text::LayoutJob::simple(shown, font.clone(), color, wrap);
        ui.fonts(|f| f.layout_job(job))
    };
    ui.add(
        TextEdit::multiline(buf)
            .id(id)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .layouter(&mut lay),
    )
}

/// A credential as dots; the scheme word stays so the user sees what kind it is. The length is
/// fixed, so the mask tells nothing about the key.
pub fn mask(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return String::new();
    }
    let dots = "\u{2022}".repeat(MASK_DOTS);
    match split_scheme(v) {
        (Some(s), _) => format!("{s} {dots}"),
        (None, _) => dots,
    }
}

/// Header line `line` as `Name: <mask>`, for the error note under the box.
pub fn masked_line(text: &str, line: usize) -> String {
    let Some((name, value)) = text.lines().nth(line).and_then(|l| l.split_once(':')) else {
        return String::new();
    };
    format!("{}: {}", name.trim(), mask(value))
}

/// The result of "Move to Key".
pub struct Moved {
    /// The headers without the credential line.
    pub text: String,
    /// The value for the key field; `None` when the key field already has one.
    pub key: Option<String>,
    /// How the key is sent from now on, read from the header that carried it.
    pub auth: Option<AuthKind>,
}

/// Takes the first credential line out of the headers. The value goes to the key field when
/// that is empty; otherwise it is dropped. Either way it never stays in the text, and it is
/// registered with the scrubber at once, so logs, errors and the chat journal mask it.
pub fn move_to_key(text: &str, key_empty: bool) -> Moved {
    let Some((line, name)) = header_issue(text) else {
        return Moved {
            text: text.to_string(),
            key: None,
            auth: None,
        };
    };
    let value = text
        .lines()
        .nth(line)
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    let _ = ddc_ai::Secret::new(value.clone());
    let rest: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != line)
        .map(|(_, l)| l)
        .collect();
    let text = rest.join("\n");
    if !key_empty {
        return Moved {
            text,
            key: None,
            auth: None,
        };
    }
    let bearer = match split_scheme(&value) {
        (Some(s), token)
            if s.eq_ignore_ascii_case("bearer") && name.eq_ignore_ascii_case("authorization") =>
        {
            Some(token.to_string())
        }
        _ => None,
    };
    let (key, auth) = match bearer {
        Some(token) => {
            let _ = ddc_ai::Secret::new(token.clone());
            (token, AuthKind::Bearer)
        }
        None => (value, AuthKind::Header { name }),
    };
    Moved {
        text,
        key: Some(key),
        auth: Some(auth),
    }
}

/// Why Save is off, as a catalog key: a credential still sits in the plain-text headers.
pub fn save_block(headers: &str) -> Option<&'static str> {
    header_issue(headers).map(|_| "ai_keys.save_blocked")
}

/// The same for connections not on screen: their headers were committed with a key in them.
pub fn profiles_block(profiles: &[ddc_ai::Profile]) -> Option<&'static str> {
    profiles
        .iter()
        .find_map(|p| save_block(&logic::headers_text(&p.extra_headers)))
}

/// Text from a failed save or store call, safe to show and to log: known keys first, then the
/// key-like patterns.
pub fn safe_error(text: &str) -> String {
    logic::mask_secrets(&ddc_ai::secret::scrub_known(text))
}

fn shown_id(id: egui::Id) -> egui::Id {
    id.with("ddc.ai.keys.shown")
}

/// Whether the masked field `id` is shown in clear. Off unless the user asked.
pub fn shown(ctx: &egui::Context, id: egui::Id) -> bool {
    ctx.data(|d| d.get_temp::<bool>(shown_id(id))).unwrap_or(false)
}

pub fn set_shown(ctx: &egui::Context, id: egui::Id, on: bool) {
    ctx.data_mut(|d| d.insert_temp(shown_id(id), on));
}

/// Drops what egui keeps of a masked field: the undo copy of its text and the shown flag.
pub fn forget(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| {
        d.remove::<egui::text_edit::TextEditState>(id);
        d.remove::<bool>(shown_id(id));
    });
}

/// A single-line key field, masked unless shown.
pub fn secret_edit(ui: &mut Ui, buf: &mut String, id: egui::Id, hint: &str) -> egui::Response {
    let masked = !shown(ui.ctx(), id);
    ui.add(
        TextEdit::singleline(buf)
            .id(id)
            .password(masked)
            .desired_width(f32::INFINITY)
            .hint_text(hint),
    )
}

/// Show or hide for the masked field `id`.
pub fn show_toggle(ui: &mut Ui, th: &Theme, cat: &Catalog, id: egui::Id) {
    let on = shown(ui.ctx(), id);
    let label = cat.t(if on { "ai_keys.hide" } else { "ai_keys.show" });
    if Button::quiet(&label).small().show(ui, th).clicked() {
        set_shown(ui.ctx(), id, !on);
    }
}

/// Show or hide, and copy of what is typed (a stored key never comes back to the form).
pub fn secret_actions(ui: &mut Ui, th: &Theme, cat: &Catalog, id: egui::Id, value: &str) {
    show_toggle(ui, th, cat, id);
    if Button::quiet(&cat.t("ai_keys.copy"))
        .small()
        .enabled(!value.is_empty())
        .show(ui, th)
        .clicked()
    {
        copy_secret(ui.ctx(), value.to_string());
    }
    if clip_pending() {
        kit::caption(ui, th, &cat.t("ai_safety.clip_clears"));
    }
}

/// How long a copied key or token may stay on the clipboard.
pub const CLIP_KEEP: std::time::Duration = std::time::Duration::from_secs(30);

/// A copied secret, known only by its digest: the value itself is not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied {
    pub digest: u64,
    pub at: web_time::Instant,
}

thread_local! {
    static COPIED: std::cell::Cell<Option<Copied>> = const { std::cell::Cell::new(None) };
}

pub fn clip_digest(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The clipboard is wiped once the time is up and only while it still holds the copy: what
/// the person copied since is left alone. `clip` is the clipboard now (`None`: unreadable).
pub fn should_clear(copied: &Copied, now: web_time::Instant, clip: Option<&str>) -> bool {
    let due = now.saturating_duration_since(copied.at) >= CLIP_KEEP;
    due && clip.is_some_and(|c| clip_digest(c) == copied.digest)
}

/// Copies a key or token and arms the 30 s wipe.
pub fn copy_secret(ctx: &egui::Context, text: String) {
    let c = Copied {
        digest: clip_digest(&text),
        at: web_time::Instant::now(),
    };
    COPIED.with(|s| s.set(Some(c)));
    ctx.copy_text(text);
    ctx.request_repaint_after(CLIP_KEEP);
}

/// A copied secret is still waiting for its wipe.
pub fn clip_pending() -> bool {
    COPIED.with(|s| s.get().is_some())
}

/// Once per frame: wipes the clipboard when its copy is due.
pub fn clip_tick(ctx: &egui::Context) {
    let Some(c) = COPIED.with(|s| s.get()) else {
        return;
    };
    let now = web_time::Instant::now();
    let left = CLIP_KEEP.saturating_sub(now.saturating_duration_since(c.at));
    if !left.is_zero() {
        ctx.request_repaint_after(left);
        return;
    }
    COPIED.with(|s| s.set(None));
    // headless and browser builds: `new` fails, no OS clipboard was ever written
    let Ok(mut board) = crate::os_clip::Clipboard::new() else {
        return;
    };
    if should_clear(&c, now, board.get_text().ok().as_deref()) {
        let _ = board.set_text(String::new());
    }
}

/// At exit the wait is cut short: the copy goes if the clipboard still holds it.
pub fn should_clear_on_exit(copied: Option<&Copied>, clip: Option<&str>) -> bool {
    copied.is_some_and(|c| clip.is_some_and(|t| clip_digest(t) == c.digest))
}

/// Called once when the app ends, on the UI thread: no frame is left to run the 30 s wipe.
pub fn clip_exit() {
    let Some(c) = COPIED.with(|s| s.take()) else {
        return;
    };
    let Ok(mut board) = crate::os_clip::Clipboard::new() else {
        return;
    };
    if should_clear_on_exit(Some(&c), board.get_text().ok().as_deref()) {
        let _ = board.set_text(String::new());
    }
}

/// A field value back to whole tokens inside the range; garbage gives the minimum.
pub fn tokens_of(v: f64, min: u32, max: u32) -> u32 {
    if !v.is_finite() {
        return min;
    }
    v.round().clamp(f64::from(min), f64::from(max)) as u32
}

pub fn preset_label(cat: &Catalog, n: u32) -> String {
    if n >= 1_000_000 && n % 1_000_000 == 0 {
        return cat.tf("ai_keys.preset_m", &[("n", &(n / 1_000_000).to_string())]);
    }
    let k = if n % 1024 == 0 { n / 1024 } else { n / 1000 };
    cat.tf("ai_keys.preset_k", &[("n", &k.to_string())])
}

/// Token limit: grouped digits, muted unit, typed or picked from the presets.
pub fn token_field(
    ui: &mut Ui,
    th: &Theme,
    cat: &Catalog,
    salt: &str,
    value: &mut u32,
    (min, max, step): (u32, u32, u32),
) {
    ui.horizontal(|ui| {
        let unit = cat.t("ai_keys.tokens");
        let mut v = f64::from(*value);
        let w = ui.available_width() * 0.55;
        NumberField::new(salt, &mut v, NumberFormat::for_lang(cat.lang))
            .labelled_by(egui::Id::new((salt, "lbl")), &unit)
            .decimals(0)
            .step(f64::from(step))
            .range(f64::from(min), f64::from(max))
            .unit(&unit)
            .width(w)
            .show(ui, th);
        *value = tokens_of(v, min, max);
        let fit: Vec<u32> = TOKEN_PRESETS
            .iter()
            .copied()
            .filter(|n| (min..=max).contains(n))
            .collect();
        let mut names = vec![cat.t("ai_keys.presets")];
        names.extend(fit.iter().map(|n| preset_label(cat, *n)));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut i = fit.iter().position(|n| *n == *value).map_or(0, |p| p + 1);
        let w = ui.available_width();
        if kit::dropdown(ui, th, &format!("{salt}.preset"), &mut i, &refs, w) && i > 0 {
            *value = fit[i - 1];
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::SecretStore;

    // Fixtures only: shaped like keys, never real ones.
    const FAKE_KEY: &str = "sk-fixture-0000-not-a-real-key-0000";
    const FAKE_OPAQUE: &str = "Zx9fixture0token0not0real0Q1w2e3r4t5";

    #[test]
    fn key_looking_headers_are_caught_by_name_and_by_value() {
        let cases = [
            format!("Authorization: Bearer {FAKE_KEY}"),
            format!("X-Api-Key: {FAKE_KEY}"),
            format!("x-custom: {FAKE_KEY}"),
            format!("x-custom: Bearer {FAKE_OPAQUE}"),
            format!("x-custom: {FAKE_OPAQUE}"),
            "Cookie: session=abc".to_string(),
            // an id-like name still refuses a key-shaped value
            format!("x-organization: {FAKE_KEY}"),
            format!("x-project: Bearer {FAKE_OPAQUE}"),
        ];
        for c in &cases {
            let text = format!("x-version: 1\n{c}");
            let (line, _) = header_issue(&text).unwrap_or_else(|| panic!("missed {c}"));
            assert_eq!(line, 1, "{c}");
        }
        assert_eq!(
            header_issue(&format!("X-Api-Key: {FAKE_KEY}")),
            Some((0, "X-Api-Key".to_string()))
        );
    }

    #[test]
    fn ordinary_headers_are_not_keys() {
        let text = "x-version: 2023-06-01\nContent-Type: application/json\nAccept: text/event-stream\n\
                    x-request-id: 123e4567-e89b-12d3-a456-426614174000\nx-plan: basic tier\n\
                    x-request-source: desktop-editor\nUser-Agent: DDC Editor/2.0\nbroken line\n";
        assert_eq!(header_issue(text), None);
        assert_eq!(save_block(text), None);
        assert_eq!(header_issue(""), None);
        // a credential name with nothing after it is still being typed, not stored
        assert_eq!(header_issue("Authorization:"), None);
        // A long random-looking id under an id-like name is not a key
        let ids = format!("x-organization: org-{FAKE_OPAQUE}\nx-project: {FAKE_OPAQUE}");
        assert_eq!(header_issue(&ids), None);
        assert_eq!(save_block(&ids), None);
    }

    #[test]
    fn move_to_key_moves_the_value_and_clears_the_line() {
        let text = format!("x-version: 1\nAuthorization: Bearer {FAKE_KEY}\nx-two: 2");
        let m = move_to_key(&text, true);
        assert_eq!(m.text, "x-version: 1\nx-two: 2");
        assert!(!m.text.contains(FAKE_KEY));
        assert_eq!(m.key.as_deref(), Some(FAKE_KEY), "the scheme is not part of the key");
        assert_eq!(m.auth, Some(AuthKind::Bearer));
        assert_eq!(header_issue(&m.text), None);
        assert_eq!(save_block(&m.text), None);

        let m = move_to_key(&format!("X-Api-Key: {FAKE_KEY}"), true);
        assert_eq!(m.text, "");
        assert_eq!(m.key.as_deref(), Some(FAKE_KEY));
        assert_eq!(
            m.auth,
            Some(AuthKind::Header {
                name: "X-Api-Key".to_string()
            })
        );
    }

    #[test]
    fn move_with_a_typed_key_drops_the_value_and_keeps_the_key() {
        let text = format!("X-Api-Key: {FAKE_KEY}\nx-version: 1");
        let m = move_to_key(&text, false);
        assert_eq!(m.text, "x-version: 1");
        assert!(m.key.is_none() && m.auth.is_none());
        let clean = "x-version: 1";
        let m = move_to_key(clean, true);
        assert_eq!(m.text, clean);
        assert!(m.key.is_none() && m.auth.is_none());
    }

    #[test]
    fn moved_value_is_masked_in_errors_logs_and_the_journal() {
        // short and pattern-free: only the scrubber's list can hide it
        let short = "fixture-k3y-q7";
        let m = move_to_key(&format!("X-Api-Key: {short}"), true);
        assert_eq!(m.key.as_deref(), Some(short));
        let line = format!("upload failed for {short} at host");
        let scrubbed = ddc_ai::secret::scrub_known(&line);
        assert!(!scrubbed.contains(short), "{scrubbed}");
        let shown = safe_error(&format!("Authorization: Bearer {FAKE_OPAQUE}; also {short}"));
        assert!(!shown.contains(FAKE_OPAQUE) && !shown.contains(short), "{shown}");
    }

    #[test]
    fn headers_box_masks_only_key_values_char_for_char() {
        let text = format!("x-version: 1\r\nAuthorization: Bearer {FAKE_KEY}\nx-two: 2\n");
        let m = mask_header_values(&text);
        assert_eq!(m.chars().count(), text.chars().count(), "the cursor maps 1:1");
        assert!(!m.contains(FAKE_KEY) && !m.contains("Bearer"), "{m}");
        let lines: Vec<&str> = m.split('\n').collect();
        assert_eq!(lines.len(), 4, "line breaks stay");
        assert_eq!(lines[0], "x-version: 1\r");
        assert!(lines[1].starts_with("Authorization: ") && lines[1].contains('\u{2022}'));
        assert_eq!(lines[2], "x-two: 2");
        assert_eq!(mask_header_values("x-version: 1"), "x-version: 1");
        let src = include_str!("settings.rs");
        assert!(!src.contains(&[".pass", "word(hide)"].concat()), "no whole-box password mask");
    }

    #[test]
    fn mask_hides_everything_but_the_scheme() {
        assert_eq!(mask(""), "");
        let m = mask(FAKE_KEY);
        assert!(!m.contains("sk-") && !m.contains("0000"));
        assert_eq!(m, mask("x"), "fixed length tells nothing");
        let b = mask(&format!("Bearer {FAKE_KEY}"));
        assert!(b.starts_with("Bearer ") && !b.contains(FAKE_KEY));
        let l = masked_line(&format!("x: 1\nX-Api-Key: {FAKE_KEY}"), 1);
        assert!(l.starts_with("X-Api-Key: ") && !l.contains(FAKE_KEY), "{l}");
    }

    #[test]
    fn masking_is_on_by_default_and_show_toggles() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("ddc.ai.keys.test_field");
        assert!(!shown(&ctx, id), "masked by default");
        set_shown(&ctx, id, true);
        assert!(shown(&ctx, id));
        set_shown(&ctx, id, false);
        assert!(!shown(&ctx, id));
        set_shown(&ctx, id, true);
        forget(&ctx, id);
        assert!(!shown(&ctx, id), "closing the form masks again");
    }

    #[test]
    fn secret_field_renders_in_both_languages() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "ai_keys.header_is_key",
                "ai_keys.move",
                "ai_keys.remove_line",
                "ai_keys.save_blocked",
                "ai_keys.show",
                "ai_keys.hide",
                "ai_keys.copy",
                "ai_keys.tokens",
                "ai_keys.presets",
                "ai_keys.images_on",
            ] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
            let ctx = ddc_ui::theme::headless_ctx();
            let id = egui::Id::new("ddc.ai.keys.render");
            let mut key = FAKE_KEY.to_string();
            let mut tokens = 131_072;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    secret_edit(ui, &mut key, id, "");
                    secret_actions(ui, &th, &cat, id, &key);
                    token_field(ui, &th, &cat, "ddc.ai.keys.t", &mut tokens, (1024, 4_000_000, 1024));
                });
            });
            assert!(!shown(&ctx, id));
            assert_eq!(key, FAKE_KEY);
            assert_eq!(tokens, 131_072);
        }
    }

    #[test]
    fn token_values_and_presets() {
        assert_eq!(tokens_of(f64::NAN, 256, 1_000_000), 256);
        assert_eq!(tokens_of(5e9, 1024, 4_000_000), 4_000_000);
        assert_eq!(tokens_of(10.0, 1024, 4_000_000), 1024);
        assert_eq!(tokens_of(32_767.6, 1024, 4_000_000), 32_768);
        let en = Catalog::new(ddc_ui::Lang::En);
        let labels: Vec<String> = TOKEN_PRESETS.iter().map(|n| preset_label(&en, *n)).collect();
        assert_eq!(labels, ["8k", "32k", "128k", "200k", "1M"]);
    }

    #[test]
    fn settings_round_trip_never_writes_the_key() {
        let d = std::env::temp_dir().join(format!("ddc-ai-keys-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut p = ddc_ai::fake::profile();
        let pasted = format!("x-version: 1\nAuthorization: Bearer {FAKE_KEY}");
        // the save the form would refuse: the crate refuses it too, and names no value
        p.extra_headers = logic::parse_headers(&pasted);
        let e = ddc_ai::profile::save_profiles(&d, &[p.clone()]).unwrap_err().to_string();
        assert!(!e.contains(FAKE_KEY), "{e}");
        assert!(!safe_error(&e).contains(FAKE_KEY));
        // after "Move to Key" the file holds the profile and the store holds the key
        let m = move_to_key(&pasted, true);
        p.extra_headers = logic::parse_headers(&m.text);
        p.auth = m.auth.clone().unwrap();
        let store = ddc_ai::MemoryStore::new();
        store.set(&p.id, ddc_ai::Secret::new(m.key.clone().unwrap())).unwrap();
        ddc_ai::profile::save_profiles(&d, &[p.clone()]).unwrap();
        let file = std::fs::read_to_string(ddc_ai::profile::profiles_path(&d)).unwrap();
        assert!(!file.contains(FAKE_KEY), "the settings file holds no key");
        assert_eq!(ddc_ai::profile::load_profiles(&d).unwrap(), vec![p.clone()]);
        assert_eq!(
            store.get(&p.id).unwrap(),
            Some(ddc_ai::Secret::new(FAKE_KEY))
        );
        assert_eq!(format!("{:?}", store.get(&p.id).unwrap()), "Some(***)");
    }

    #[test]
    fn settings_form_uses_kit_widgets_and_hides_header_keys() {
        let src = include_str!("settings.rs");
        let raw_check = ["ui", ".checkbox("].concat();
        let raw_drag = ["egui::", "DragValue"].concat();
        assert!(!src.contains(&raw_check), "images uses the kit checkbox");
        assert!(!src.contains(&raw_drag), "token limits use the number field");
        assert!(src.contains("keys::save_block("), "Save knows why it is off");
        assert!(src.contains("keys::secret_edit("), "the key field is masked");
        assert!(src.contains("keys::safe_error("), "save errors are masked");
    }

    #[test]
    fn a_copied_key_is_wiped_only_when_due_and_still_on_the_clipboard() {
        let at = web_time::Instant::now();
        let c = Copied {
            digest: clip_digest("fake-SECRETSECRET-clip"),
            at,
        };
        let due = at + CLIP_KEEP;
        assert!(!should_clear(&c, at, Some("fake-SECRETSECRET-clip")), "not yet");
        assert!(should_clear(&c, due, Some("fake-SECRETSECRET-clip")));
        assert!(!should_clear(&c, due, Some("something the person copied")));
        assert!(!should_clear(&c, due, None), "an unreadable clipboard is left alone");
    }

    #[test]
    fn exit_wipes_a_pending_copy() {
        let c = Copied {
            digest: clip_digest(FAKE_KEY),
            at: web_time::Instant::now(),
        };
        // well before the 30 s are up
        assert!(should_clear_on_exit(Some(&c), Some(FAKE_KEY)));
        assert!(!should_clear_on_exit(Some(&c), Some("something the person copied")));
        assert!(!should_clear_on_exit(Some(&c), None), "an unreadable clipboard is left alone");
        assert!(!should_clear_on_exit(None, Some(FAKE_KEY)), "nothing was copied here");
        // the exit hook forgets the copy even where no clipboard is reachable
        COPIED.with(|s| s.set(Some(c)));
        clip_exit();
        assert!(!clip_pending());
        assert!(!format!("{c:?}").contains(FAKE_KEY));
    }

    #[test]
    fn copy_arms_the_wipe_and_says_so_in_both_languages() {
        let ctx = egui::Context::default();
        copy_secret(&ctx, "fake-SECRETSECRET-arm".into());
        assert!(clip_pending());
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in ["ai_safety.clip_clears", "ai_safety.copied_clears"] {
                assert_ne!(cat.t(k), k, "{k}");
            }
        }
    }
}
