//! Design tokens (docs/design/tokens.json, embedded at build time) → egui `Style`.
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, Vec2};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;

pub use crate::sem::{
    control, icon_size, layout, radius, sem, sem_of, space, stroke, type_size, ControlSize, Sem,
};

pub const TOKENS_JSON: &str = include_str!("../../../docs/design/tokens.json");
/// UI scale steps from ONBOARDING_FEEDBACK §4.4.
pub const SCALE_STEPS: [f32; 8] = [0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub color: Palettes,
    pub space: Space,
    pub radius: BTreeMap<String, f32>,
    pub density: Densities,
    pub icon: IconTokens,
    pub font: serde_json::Value,
    pub elevation: serde_json::Value,
    pub breakpoint: Breakpoints,
    #[serde(default)]
    pub viewport: serde_json::Value,
}
#[derive(Debug, Deserialize, Clone, Copy)]
pub struct Breakpoints {
    pub compact_below: f32,
    /// Below this width the ribbon shows icons only; above it labels stay (progressive).
    pub icons_below: f32,
    pub full_from: f32,
}

pub use crate::typo::{Role, Spec, Weight};

/// Width class of the window in points (DESIGN_SYSTEM "Breakpoints").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Width {
    Compact,
    Regular,
    Full,
}

/// Product face alias (`tokens.font.family.ui`); the licence name stays in assets/fonts/OFL.txt.
pub const W_REGULAR: &str = "DDC Sans";
pub const W_MEDIUM: &str = "DDC Sans Medium";
pub const W_SEMIBOLD: &str = "DDC Sans SemiBold";
/// Short motion (hover, press, panel toggles), seconds.
pub const MOTION_FAST_S: f32 = 0.12;

/// Ribbon geometry in points (`tokens.json` `ribbon`, checked by `ribbon_constants_match_json`).
/// Every value is whole, so buttons, icons and hairlines land on pixels at 1x and 2x.
pub mod ribbon {
    /// Tab strip height.
    pub const TAB_H: f32 = 32.0;
    /// Space above the button band.
    pub const PAD_Y: f32 = 4.0;
    /// Button band: one large button or a stack of three rows.
    pub const BAND_H: f32 = 72.0;
    pub const ROW_H: f32 = 24.0;
    pub const ROWS: usize = 3;
    /// Group title row under the band (title and optional launcher).
    pub const TITLE_H: f32 = 16.0;
    pub const LARGE_ICON: f32 = 24.0;
    pub const MEDIUM_ICON: f32 = 16.0;
    /// Large button: icon top inset, label top, label line height.
    pub const LARGE_ICON_TOP: f32 = 8.0;
    pub const LABEL_TOP: f32 = 32.0;
    pub const LINE_H: f32 = 14.0;
    pub const LARGE_MIN_W: f32 = 48.0;
    pub const LARGE_PAD_X: f32 = 8.0;
    /// A label up to this width stays on one line; wider ones wrap into two balanced lines.
    pub const ONE_LINE_MAX: f32 = 60.0;
    /// Caption caps at the ribbon label size: large two-line block, medium one line.
    /// Longer captions are elided (full name in the tooltip); shipped locales stay under them.
    pub const LARGE_LABEL_MAX: f32 = 100.0;
    pub const MEDIUM_LABEL_MAX: f32 = 144.0;
    pub const MEDIUM_PAD_L: f32 = 4.0;
    pub const MEDIUM_GAP: f32 = 8.0;
    pub const MEDIUM_PAD_R: f32 = 8.0;
    pub const SMALL_W: f32 = 24.0;
    /// Caret zone of a split button.
    pub const CARET_W: f32 = 16.0;
    pub const COL_GAP: f32 = 2.0;
    pub const GROUP_PAD_X: f32 = 4.0;
    /// Space between groups; the hairline separator sits in its middle.
    pub const GROUP_GAP: f32 = 8.0;
    pub const LAUNCHER_W: f32 = 16.0;
    pub const TITLE_PAD_X: f32 = 8.0;
    /// Trailing menu of groups that do not fit even collapsed.
    pub const OVERFLOW_W: f32 = 72.0;
    /// Inset of the band from the window edges.
    pub const EDGE: f32 = 8.0;
    /// Key tip badge height.
    pub const TIP_H: f32 = 16.0;
    /// Minimum width of a caret, group or overflow menu.
    pub const MENU_W: f32 = 220.0;
    /// Hover and press fade, seconds (ease-out).
    pub const MOTION_S: f32 = 0.12;
}

pub fn family(name: &str) -> FontFamily {
    if name == W_REGULAR {
        FontFamily::Proportional
    } else {
        FontFamily::Name(name.into())
    }
}

/// UI typeface. The weights keep their family names whichever face draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UiFont {
    /// DDC Sans (SIL OFL 1.1, assets/fonts/OFL.txt); stored key stays "inter".
    #[default]
    Sans,
    /// IBM Plex Sans (SIL OFL 1.1, assets/fonts/OFL-IBMPlexSans.txt).
    Plex,
    /// The operating system's interface face; DDC Sans where it cannot be read.
    System,
}

impl UiFont {
    pub const ALL: [UiFont; 3] = [UiFont::Sans, UiFont::Plex, UiFont::System];
    pub fn key(self) -> &'static str {
        match self {
            Self::Sans => "inter",
            Self::Plex => "plex",
            Self::System => "system",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == s)
    }
}

const SANS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/DDCSans-Regular.ttf"),
    include_bytes!("../assets/fonts/DDCSans-Medium.ttf"),
    include_bytes!("../assets/fonts/DDCSans-SemiBold.ttf"),
];
const PLEX: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
];

/// Regular, medium and semibold of the system face, read once; None off Windows or when a
/// file is missing.
fn system_faces() -> Option<&'static [&'static [u8]; 3]> {
    static F: std::sync::OnceLock<Option<[&'static [u8]; 3]>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let dir = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
        let read = |n: &str| -> Option<&'static [u8]> {
            Some(Box::leak(std::fs::read(dir.join(n)).ok()?.into_boxed_slice()))
        };
        // the system face has no medium cut: labels take its semibold
        let (r, sb) = (read("segoeui.ttf")?, read("seguisb.ttf")?);
        Some([r, sb, sb])
    })
    .as_ref()
}

/// Bundled UI font (SIL OFL 1.1, crates/ui/assets/fonts/OFL.txt): three weights with tabular
/// figures baked into the default digits, so every number column aligns.
pub fn font_definitions() -> egui::FontDefinitions {
    font_definitions_for(UiFont::Sans)
}

/// Font set drawn with `face`; family names stay the same so roles need no change.
pub fn font_definitions_for(face: UiFont) -> egui::FontDefinitions {
    font_definitions_for_lang(face, crate::i18n::current())
}

/// `font_definitions_for` plus the script fallbacks `lang` needs (`i18n_fonts`).
pub fn font_definitions_for_lang(face: UiFont, lang: crate::i18n::Lang) -> egui::FontDefinitions {
    // Only our own faces: the toolkit's built-in fonts are not compiled in (`default_fonts` off).
    let mut fd = egui::FontDefinitions::empty();
    let bytes = match face {
        UiFont::Sans => &SANS,
        UiFont::Plex => &PLEX,
        UiFont::System => system_faces().unwrap_or(&SANS),
    };
    let data: [(&str, &'static [u8]); 3] = [
        (W_REGULAR, bytes[0]),
        (W_MEDIUM, bytes[1]),
        (W_SEMIBOLD, bytes[2]),
    ];
    for (name, bytes) in data {
        fd.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
        fd.families.insert(family(name), vec![name.to_owned()]);
    }
    fd.families.insert(FontFamily::Monospace, vec![W_REGULAR.to_owned()]);
    crate::i18n_fonts::add_to(&mut fd, lang);
    fd
}

/// Installs the bundled fonts once per context. Returns false on the pass that installed them:
/// the named weights exist only from the next pass, so the caller skips drawing (and discards).
pub fn install_fonts(ctx: &egui::Context) -> bool {
    install_fonts_for(ctx, UiFont::Sans)
}

/// `install_fonts` with a chosen face; a different face than the installed one reinstalls.
pub fn install_fonts_for(ctx: &egui::Context, face: UiFont) -> bool {
    let id = egui::Id::new("ddc.fonts");
    // the script fallbacks follow the interface language: a language switch reinstalls only when
    // its font set differs
    let key = (face, crate::i18n_fonts::set_key(crate::i18n::current()));
    if ctx.data(|d| d.get_temp::<(UiFont, u16)>(id)) == Some(key) {
        return true;
    }
    ctx.set_fonts(font_definitions_for(face));
    ctx.data_mut(|d| d.insert_temp(id, key));
    ctx.request_discard("ddc fonts installed");
    false
}

/// Headless context (tests, probes) with every kit family bound before the first drawn pass:
/// a bare context panics on the named weights (`Role::Label`, section and table headers).
pub fn headless_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    ctx
}

/// `#RRGGBBAA` overlay colour flattened onto an opaque background.
pub fn over_rgba(fg: Color32, bg: Color32) -> Color32 {
    let [r, g, b, a] = fg.to_srgba_unmultiplied();
    over(Color32::from_rgb(r, g, b), a as f32 / 255.0, bg)
}
#[derive(Debug, Clone, Deserialize)]
pub struct Palettes {
    pub light: BTreeMap<String, String>,
    pub dark: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Space {
    pub unit: f32,
    pub scale: Vec<f32>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Densities {
    pub dense: DensityTokens,
    pub compact: DensityTokens,
    pub comfortable: DensityTokens,
}
#[derive(Debug, Deserialize, Clone, Copy)]
pub struct DensityTokens {
    pub row: f32,
    pub control: f32,
    pub control_sm: f32,
    pub control_lg: f32,
    pub icon: f32,
    pub pad_x: f32,
    pub gap: f32,
    pub toolbar: f32,
}
#[derive(Debug, Clone, Deserialize)]
pub struct IconTokens {
    pub grid: u32,
    pub stroke: f32,
    pub sizes: Vec<u32>,
}

#[derive(Debug)]
pub enum TokenError {
    Json(serde_json::Error),
    BadColor { name: String, value: String },
    Missing(String),
}
impl std::error::Error for TokenError {}
impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "tokens.json: {e}"),
            Self::BadColor { name, value } => write!(f, "tokens.json: color {name} = {value:?}"),
            Self::Missing(n) => write!(f, "tokens.json: missing color {n}"),
        }
    }
}

impl Tokens {
    pub fn embedded() -> Result<Self, TokenError> {
        serde_json::from_str(TOKENS_JSON).map_err(TokenError::Json)
    }
    pub fn palette(&self, mode: Mode) -> &BTreeMap<String, String> {
        match mode {
            Mode::Light => &self.color.light,
            Mode::Dark => &self.color.dark,
        }
    }
    pub fn color(&self, mode: Mode, name: &str) -> Result<Color32, TokenError> {
        let v = self
            .palette(mode)
            .get(name)
            .ok_or_else(|| TokenError::Missing(name.into()))?;
        parse_hex(v).ok_or_else(|| TokenError::BadColor {
            name: name.into(),
            value: v.clone(),
        })
    }
}

/// `#RRGGBB` or `#RRGGBBAA` (straight alpha).
pub fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#')?;
    if !h.is_ascii() || (h.len() != 6 && h.len() != 8) {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let a = if h.len() == 8 { b(6)? } else { 255 };
    Some(Color32::from_rgba_unmultiplied(b(0)?, b(2)?, b(4)?, a))
}

/// WCAG 2.x relative luminance of an opaque colour.
pub fn luminance(c: Color32) -> f64 {
    let ch = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}
/// Focus ring geometry (`tokens.focus.ring`), shared by every widget.
pub mod focus {
    use egui::{CornerRadius, Rect};
    pub const WIDTH: f32 = 2.0;
    pub const OFFSET: f32 = 2.0;
    /// Inner edge of the ring (stroke drawn outside it) and its corner radius.
    pub fn ring(control: Rect, radius: f32) -> (Rect, CornerRadius) {
        let r = (radius.max(0.0) + OFFSET).round().clamp(0.0, 255.0) as u8;
        (control.expand(OFFSET), CornerRadius::same(r))
    }
}

/// Colour `fg` attenuated by alpha `a`, composited over the opaque `bg`.
pub fn over(fg: Color32, a: f32, bg: Color32) -> Color32 {
    let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    Color32::from_rgb(m(fg.r(), bg.r()), m(fg.g(), bg.g()), m(fg.b(), bg.b()))
}

pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// The person's «compact»: tighter than the product default.
    Dense,
    /// The product default («normal» in settings).
    Compact,
    Comfortable,
}

/// Resolved theme: what widgets read at draw time.
#[derive(Clone)]
pub struct Theme {
    pub tokens: Tokens,
    pub mode: Mode,
    pub density: Density,
    /// UI scale, one of `SCALE_STEPS` (applied as egui zoom factor).
    pub scale: f32,
    /// Motion off (system setting or preference): animations become instant.
    pub reduced_motion: bool,
    /// Corner-radius multiplier from the layout (`TokenOverrides::radius_scale`).
    pub radius_scale: f32,
    /// Interface face (settings "Appearance").
    pub font: UiFont,
    /// Text size factor over the type roles, 0.85–1.25.
    pub text_scale: f32,
    /// Semantic palettes (light, dark) of `tokens`, kept in step by `set_palette`.
    sem_live: Arc<[Sem; 2]>,
    /// Bumped whenever `set_palette` changes colours: the style is rebuilt.
    palette_gen: u32,
    /// Patches currently over the embedded palettes.
    palette_key: Option<Arc<(BTreeMap<String, String>, BTreeMap<String, String>)>>,
    applied_zoom: f32,
    applied_style: Option<(Mode, Density, bool, u32, UiFont, u32, u32)>,
}

impl Theme {
    pub fn new(mode: Mode, density: Density) -> Self {
        let tokens = Tokens::embedded().expect("embedded tokens.json is validated by tests");
        Self {
            tokens,
            mode,
            density,
            scale: 1.0,
            reduced_motion: false,
            radius_scale: 1.0,
            font: UiFont::Sans,
            text_scale: 1.0,
            sem_live: Arc::new([*sem(false), *sem(true)]),
            palette_gen: 0,
            palette_key: None,
            applied_zoom: 1.0,
            applied_style: None,
        }
    }
    /// Typed semantic palette of the current mode.
    pub fn sem(&self) -> &Sem {
        &self.sem_live[usize::from(self.mode == Mode::Dark)]
    }
    /// Colours over the embedded palettes (light, dark): a look and its accent. Unknown names
    /// and bad values are ignored, so a patch can never blank a colour. No-op when unchanged.
    pub fn set_palette(&mut self, light: &BTreeMap<String, String>, dark: &BTreeMap<String, String>) {
        if let Some(k) = &self.palette_key {
            if k.0 == *light && k.1 == *dark {
                return;
            }
        }
        let mut t = Tokens::embedded().expect("embedded tokens.json is validated by tests");
        for (pal, patch) in [(&mut t.color.light, light), (&mut t.color.dark, dark)] {
            for (k, v) in patch {
                if pal.contains_key(k) && parse_hex(v).is_some() {
                    pal.insert(k.clone(), v.clone());
                }
            }
        }
        let of = |m| Sem::resolve(&t, m).expect("patched values are parsed above");
        self.sem_live = Arc::new([of(Mode::Light), of(Mode::Dark)]);
        self.tokens.color = t.color;
        self.palette_key = Some(Arc::new((light.clone(), dark.clone())));
        self.palette_gen = self.palette_gen.wrapping_add(1);
    }
    /// Control height of the current density.
    pub fn control_h(&self, size: ControlSize) -> f32 {
        let d = self.d();
        match size {
            ControlSize::Small => d.control_sm,
            ControlSize::Medium => d.control,
            ControlSize::Large => d.control_lg,
        }
    }
    /// Colour by token name; unknown names render magenta so they are visible in QA.
    pub fn c(&self, name: &str) -> Color32 {
        self.tokens
            .color(self.mode, name)
            .unwrap_or(Color32::from_rgb(255, 0, 255))
    }
    /// Drawing line weight `viewport.line_px.<name>` in points (1.0 if absent).
    pub fn line_px(&self, name: &str) -> f32 {
        self.tokens.viewport["line_px"][name]
            .as_f64()
            .unwrap_or(1.0) as f32
    }
    pub fn d(&self) -> DensityTokens {
        match self.density {
            Density::Dense => self.tokens.density.dense,
            Density::Compact => self.tokens.density.compact,
            Density::Comfortable => self.tokens.density.comfortable,
        }
    }
    /// Window width class from the screen width in points.
    pub fn width_class(&self, w: f32) -> Width {
        let b = self.tokens.breakpoint;
        if w < b.compact_below {
            Width::Compact
        } else if w < b.full_from {
            Width::Regular
        } else {
            Width::Full
        }
    }
    /// Ribbon width class: labels stay down to `icons_below`, icons only under it.
    pub fn ribbon_class(&self, w: f32) -> Width {
        if w < self.tokens.breakpoint.icons_below {
            Width::Compact
        } else if w < self.tokens.breakpoint.full_from {
            Width::Regular
        } else {
            Width::Full
        }
    }
    pub fn icon_px(&self) -> u32 {
        self.d().icon as u32
    }
    fn r(&self, name: &str) -> CornerRadius {
        CornerRadius::same(self.radius(name).round().clamp(0.0, 255.0) as u8)
    }
    /// Snap to the nearest documented step (75–200 %).
    pub fn set_scale(&mut self, s: f32) {
        self.scale = *SCALE_STEPS
            .iter()
            .min_by(|a, b| (*a - s).abs().total_cmp(&(*b - s).abs()))
            .unwrap_or(&1.0);
    }

    fn elevation_of(&self, level: u8) -> &serde_json::Value {
        let m = if self.mode == Mode::Dark { "dark" } else { "light" };
        &self.tokens.elevation[m][level.to_string()]
    }
    /// Elevation level 1–3 of the current theme (`elevation.light|dark`) → drop shadow.
    pub fn shadow(&self, level: u8) -> Shadow {
        let e = self.elevation_of(level);
        let f = |k: &str| e[k].as_f64().unwrap_or(0.0);
        let a = f("alpha").min(0.6);
        let c = self.c("shadow");
        Shadow {
            offset: [0, f("y") as i8],
            blur: f("blur") as u8,
            spread: 0,
            color: Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8),
        }
    }
    /// Outline that carries an elevation where a shadow cannot show (dark levels 2–3).
    pub fn elevation_outline(&self, level: u8) -> Option<Stroke> {
        let name = self.elevation_of(level)["outline"].as_str()?;
        Some(Stroke::new(stroke::HAIRLINE, self.c(name)))
    }
    /// The one focus ring: `focus::WIDTH` wide, `focus::OFFSET` outside `rect`, corners following
    /// the control's `radius`. Callers show it only for keyboard focus (`widgets::focus_visible`).
    pub fn paint_focus_ring(&self, painter: &egui::Painter, rect: egui::Rect, radius: f32) {
        let (r, cr) = focus::ring(rect, radius);
        painter.rect_stroke(
            r,
            cr,
            Stroke::new(focus::WIDTH, self.c("focus.ring")),
            egui::StrokeKind::Outside,
        );
    }
    /// Disabled controls: flat colours, no alpha over them (`Visuals::disabled_alpha` is 1).
    pub fn disabled_text(&self) -> Color32 {
        self.c("text.disabled")
    }
    pub fn disabled_border(&self) -> Color32 {
        self.c("border.subtle")
    }
    pub fn radius(&self, name: &str) -> f32 {
        let k = if self.radius_scale.is_finite() {
            self.radius_scale.clamp(0.0, 4.0)
        } else {
            1.0
        };
        self.tokens.radius.get(name).copied().unwrap_or(0.0) * k
    }
    /// Font for a type role (docs/design/TYPOGRAPHY.md).
    pub fn font(&self, role: Role) -> FontId {
        let f = crate::typo::font(role);
        if (self.text_scale - 1.0).abs() < 1e-3 || !self.text_scale.is_finite() {
            return f;
        }
        FontId::new(f.size * self.text_scale.clamp(0.85, 1.25), f.family)
    }
    /// Colour of a role in the current mode.
    pub fn role_color(&self, role: Role) -> Color32 {
        self.c(role.spec().color)
    }
    /// Line height of a role in points.
    pub fn line_h(&self, role: Role) -> f32 {
        role.spec().line
    }
    /// Text styled by its role: font, colour, tracking, line height and case.
    pub fn rich(&self, role: Role, text: &str) -> egui::RichText {
        let sp = role.spec();
        egui::RichText::new(crate::typo::cased(role, text))
            .font(self.font(role))
            .color(self.c(sp.color))
            .extra_letter_spacing(sp.tracking)
            .line_height(Some(sp.line))
    }
    /// Hover / pressed fills. Translucent, so the same step shows on panels, dialogs and menus.
    pub fn hover_fill(&self) -> Color32 {
        self.c("bg.hover")
    }
    pub fn pressed_fill(&self) -> Color32 {
        self.c("bg.pressed")
    }

    pub fn style(&self) -> egui::Style {
        use egui::TextStyle;
        let mut st = egui::Style::default();
        let d = self.d();
        let mut v = match self.mode {
            Mode::Light => egui::Visuals::light(),
            Mode::Dark => egui::Visuals::dark(),
        };
        let (txt, txt2) = (self.c("text.primary"), self.c("text.secondary"));
        let panel = self.c("bg.panel");
        let elevated = self.c("bg.elevated");
        v.panel_fill = panel;
        v.window_fill = elevated;
        v.extreme_bg_color = self.c("bg.input");
        v.faint_bg_color = self.c("bg.app");
        v.code_bg_color = self.c("bg.sunken");
        v.hyperlink_color = self.c("accent");
        v.warn_fg_color = self.c("warning");
        v.error_fg_color = self.c("error");
        v.selection.bg_fill = self.c("selection.bg");
        v.selection.stroke = Stroke::new(1.0, self.c("accent"));
        v.window_stroke = Stroke::new(stroke::HAIRLINE, self.c("border.overlay"));
        v.window_corner_radius = self.r("lg");
        v.menu_corner_radius = self.r("md");
        v.window_shadow = self.shadow(3);
        v.popup_shadow = self.shadow(2);
        v.text_cursor.stroke = Stroke::new(1.5, self.c("accent"));
        v.striped = false;
        let rad = self.r("md");
        // egui reads its text contrast off these fills: opaque here, over the raised rest fill,
        // since over the panel a dark hover lands on bg.elevated and the state disappears
        let (hover, press) = (
            over_rgba(self.hover_fill(), elevated),
            over_rgba(self.pressed_fill(), elevated),
        );
        let w = &mut v.widgets;
        let rows = [
            (&mut w.noninteractive, panel, "border.subtle", txt),
            (&mut w.inactive, panel, "border.control", txt),
            (&mut w.hovered, hover, "text.secondary", txt),
            (&mut w.active, press, "accent", txt),
            (&mut w.open, hover, "text.secondary", txt),
        ];
        for (ws, fill, stroke, fg) in rows {
            ws.bg_fill = fill;
            ws.weak_bg_fill = fill;
            ws.bg_stroke = Stroke::new(1.0, self.c(stroke));
            ws.fg_stroke = Stroke::new(1.0, fg);
            ws.corner_radius = rad;
            ws.expansion = 0.0;
        }
        // keyboard focus: accent outline (`widgets::focus_ring` is drawn in addition)
        w.active.bg_stroke = Stroke::new(1.0, self.c("accent"));
        // disabled is drawn with text.disabled / border.subtle; fading on top would dim twice
        v.disabled_alpha = 1.0;
        w.inactive.fg_stroke = Stroke::new(1.0, txt2);
        // a resting egui button matches the kit's secondary button: raised above the panel
        w.inactive.weak_bg_fill = elevated;
        st.visuals = v;
        let s = &mut st.spacing;
        s.item_spacing = Vec2::new(d.gap * 1.5, d.gap);
        s.button_padding = Vec2::new(d.pad_x, ((d.control - 16.0) / 2.0).max(2.0));
        s.interact_size = Vec2::new(d.control * 2.0, d.control);
        s.icon_width = control::CHECK;
        s.icon_width_inner = space::SM;
        s.icon_spacing = space::SM;
        s.window_margin = Margin::same(space::MD as i8);
        s.menu_margin = Margin::same(6);
        s.indent = space::LG;
        s.tooltip_width = layout::TOOLTIP_W;
        let mut sc = egui::style::ScrollStyle::floating();
        sc.bar_width = 8.0;
        sc.floating_width = 3.0;
        s.scroll = sc;
        st.text_styles = [
            (TextStyle::Small, self.font(Role::Caption)),
            (TextStyle::Body, self.font(Role::Body)),
            (TextStyle::Button, self.font(Role::Body)),
            (TextStyle::Heading, self.font(Role::DialogTitle)),
            (TextStyle::Monospace, self.font(Role::Code)),
        ]
        .into();
        st.interaction.tooltip_delay = 0.5;
        st.animation_time = if self.reduced_motion {
            0.0
        } else {
            MOTION_FAST_S
        };
        st
    }

    /// Applies style and scale. A zoom changed outside (built-in Ctrl+=/-) is adopted into
    /// `scale`; egui is only written when our own scale changed.
    /// Returns false on the first pass of a context (fonts pending): draw nothing this pass.
    pub fn apply(&mut self, ctx: &egui::Context) -> bool {
        let ready = install_fonts_for(ctx, self.font);
        let key = (
            self.mode,
            self.density,
            self.reduced_motion,
            self.radius_scale.to_bits(),
            self.font,
            self.text_scale.to_bits(),
            self.palette_gen,
        );
        // a new context or the egui system theme gives the default style: inputs and buttons
        // without their own colours would be filled black in the light theme — set both slots
        let et = match self.mode {
            Mode::Light => egui::Theme::Light,
            Mode::Dark => egui::Theme::Dark,
        };
        let stale = ctx.style().visuals.panel_fill != self.c("bg.panel");
        if self.applied_style != Some(key) || ctx.theme() != et || stale {
            ctx.set_theme(et);
            let st = std::sync::Arc::new(self.style());
            ctx.set_style_of(egui::Theme::Light, st.clone());
            ctx.set_style_of(egui::Theme::Dark, st);
            self.applied_style = Some(key);
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(SNAPSHOT_ID), Arc::new(self.clone())));
        }
        // the scale lives in settings only: the built-in Ctrl+0 / Ctrl+- zoom would bypass it
        if ctx.options(|o| o.zoom_with_keyboard) {
            ctx.options_mut(|o| o.zoom_with_keyboard = false);
        }
        let z = ctx.zoom_factor();
        if (z - self.applied_zoom).abs() > 1e-4 {
            self.set_scale(z);
            self.applied_zoom = z;
        }
        if (self.scale - z).abs() > 1e-4 {
            ctx.set_zoom_factor(self.scale);
            self.applied_zoom = self.scale;
        }
        ready
    }
}

const SNAPSHOT_ID: &str = "ddc.theme.snapshot";

/// Theme applied to this context, for code that holds only a `Ui`: `theme::of(ui.ctx())`.
/// Before the first `Theme::apply` it follows the context's dark mode with default density.
pub fn of(ctx: &egui::Context) -> Arc<Theme> {
    let id = egui::Id::new(SNAPSHOT_ID);
    if let Some(t) = ctx.data(|d| d.get_temp::<Arc<Theme>>(id)) {
        return t;
    }
    let mode = if ctx.style().visuals.dark_mode {
        Mode::Dark
    } else {
        Mode::Light
    };
    let t = Arc::new(Theme::new(mode, Density::Compact));
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_follows_apply() {
        let ctx = egui::Context::default();
        let mut th = Theme::new(Mode::Dark, Density::Comfortable);
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
        assert_eq!(of(&ctx).mode, Mode::Dark);
        assert_eq!(of(&ctx).density, Density::Comfortable);
        th.mode = Mode::Light;
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
        assert_eq!(of(&ctx).mode, Mode::Light);
    }

    #[test]
    fn control_heights_are_ordered() {
        for dens in [Density::Compact, Density::Comfortable] {
            let th = Theme::new(Mode::Light, dens);
            let (s, m, l) = (
                th.control_h(ControlSize::Small),
                th.control_h(ControlSize::Medium),
                th.control_h(ControlSize::Large),
            );
            assert!(s < m && m < l, "{s} {m} {l}");
        }
    }

    #[test]
    fn radius_scale_multiplies_tokens() {
        let mut th = Theme::new(Mode::Light, Density::Compact);
        let r = th.radius("lg");
        th.radius_scale = 2.0;
        assert_eq!(th.radius("lg"), r * 2.0);
        th.radius_scale = f32::NAN;
        assert_eq!(th.radius("lg"), r);
    }

    #[test]
    fn ribbon_constants_match_json() {
        let v: serde_json::Value = serde_json::from_str(TOKENS_JSON).unwrap();
        let r = &v["ribbon"];
        let n = |k: &str| r[k].as_f64().unwrap_or_else(|| panic!("ribbon.{k}")) as f32;
        use super::ribbon as k;
        for (name, c) in [
            ("tab_h", k::TAB_H),
            ("pad_y", k::PAD_Y),
            ("band_h", k::BAND_H),
            ("row_h", k::ROW_H),
            ("rows", k::ROWS as f32),
            ("title_h", k::TITLE_H),
            ("large_icon", k::LARGE_ICON),
            ("medium_icon", k::MEDIUM_ICON),
            ("large_icon_top", k::LARGE_ICON_TOP),
            ("label_top", k::LABEL_TOP),
            ("line_h", k::LINE_H),
            ("large_min_w", k::LARGE_MIN_W),
            ("large_pad_x", k::LARGE_PAD_X),
            ("one_line_max", k::ONE_LINE_MAX),
            ("large_label_max", k::LARGE_LABEL_MAX),
            ("medium_label_max", k::MEDIUM_LABEL_MAX),
            ("medium_pad_l", k::MEDIUM_PAD_L),
            ("medium_gap", k::MEDIUM_GAP),
            ("medium_pad_r", k::MEDIUM_PAD_R),
            ("small_w", k::SMALL_W),
            ("caret_w", k::CARET_W),
            ("col_gap", k::COL_GAP),
            ("group_pad_x", k::GROUP_PAD_X),
            ("group_gap", k::GROUP_GAP),
            ("launcher_w", k::LAUNCHER_W),
            ("title_pad_x", k::TITLE_PAD_X),
            ("overflow_w", k::OVERFLOW_W),
            ("edge", k::EDGE),
            ("tip_h", k::TIP_H),
            ("menu_w", k::MENU_W),
        ] {
            assert_eq!(n(name), c, "ribbon.{name}");
            assert_eq!(c.fract(), 0.0, "ribbon.{name} must be whole points");
        }
        assert_eq!(n("motion_ms") / 1000.0, k::MOTION_S);
        assert_eq!(k::ROWS as f32 * k::ROW_H, k::BAND_H, "three rows fill the band");
        assert!(k::LABEL_TOP + 2.0 * k::LINE_H <= k::BAND_H, "two label lines fit");
        for c in r["states"].as_object().unwrap().values() {
            let t = Tokens::embedded().unwrap();
            for mode in [Mode::Light, Mode::Dark] {
                assert!(t.color(mode, c.as_str().unwrap()).is_ok(), "{c}");
            }
        }
    }

    #[test]
    fn tokens_parse() {
        let t = Tokens::embedded().unwrap();
        for mode in [Mode::Light, Mode::Dark] {
            for name in t.palette(mode).keys() {
                t.color(mode, name).unwrap();
            }
        }
        assert_eq!(
            t.color.light.keys().collect::<Vec<_>>(),
            t.color.dark.keys().collect::<Vec<_>>()
        );
        assert_eq!(t.icon.grid, 24);
        assert_eq!(t.icon.sizes, vec![12, 16, 20, 24, 32]);
        assert_eq!(*t.icon.sizes.last().unwrap(), crate::icons::LARGE);
        assert_eq!(t.density.compact.icon, 20.0);
        let th = Theme::new(Mode::Light, Density::Compact);
        for lvl in 1..=3 {
            assert!(th.shadow(lvl).blur > 0, "elevation {lvl}");
        }
        assert!(parse_hex("#12345").is_none() && parse_hex(&format!("#{}", crate::test_fx::fx("bad_hex"))).is_none());
    }

    /// Pairs and thresholds from VISUAL_LANGUAGE "Contrast", recomputed from tokens.
    #[test]
    fn contrast_aa() {
        let t = Tokens::embedded().unwrap();
        let pairs: &[(&str, &str, f64)] = &[
            ("text.primary", "bg.panel", 4.5),
            ("text.secondary", "bg.panel", 4.5),
            ("text.secondary", "bg.app", 4.5),
            ("accent.text_on", "accent", 4.5),
            ("error", "bg.panel", 4.5),
            ("warning", "bg.panel", 4.5),
            ("success", "bg.panel", 4.5),
            ("selection", "bg.viewport", 3.0),
            ("hover", "bg.viewport", 3.0),
            ("preview", "bg.viewport", 3.0),
            ("snap", "bg.viewport", 3.0),
            ("disc.arch", "bg.viewport", 3.0),
            ("disc.struct", "bg.viewport", 3.0),
            ("disc.mep.mech", "bg.viewport", 3.0),
            ("disc.mep.elec", "bg.viewport", 3.0),
            ("disc.mep.plumb", "bg.viewport", 3.0),
            ("border.control", "bg.panel", 3.0),
            ("border.control", "bg.input", 3.0),
            ("text.secondary", "bg.input", 3.0),
            ("text.secondary", "bg.sunken", 4.5),
            ("text.secondary", "bg.input", 4.5),
            ("text.secondary", "bg.elevated", 4.5),
            ("text.secondary", "selection.bg", 4.5),
            ("text.placeholder", "bg.input", 4.5),
            ("text.placeholder", "bg.panel", 4.5),
            ("text.placeholder", "bg.sunken", 4.5),
            ("text.primary", "bg.sunken", 4.5),
            ("selection.text", "selection.bg", 4.5),
            ("focus", "bg.input", 3.0),
        ];
        for mode in [Mode::Light, Mode::Dark] {
            for (fg, bg, min) in pairs {
                let r = contrast(t.color(mode, fg).unwrap(), t.color(mode, bg).unwrap());
                assert!(r >= *min, "{mode:?} {fg} on {bg}: {r:.2} < {min}");
            }
        }
        // disabled text is flat text.disabled on every control surface, never faded again
        for mode in [Mode::Light, Mode::Dark] {
            for bg in ["bg.panel", "bg.elevated", "bg.input", "bg.sunken"] {
                let r = contrast(t.color(mode, "text.disabled").unwrap(), t.color(mode, bg).unwrap());
                assert!(r >= 3.0, "{mode:?} text.disabled on {bg}: {r:.2} < 3");
            }
        }
        assert_eq!(Theme::new(Mode::Light, Density::Compact).style().visuals.disabled_alpha, 1.0);
        // Spot-check against the documented table (two decimals).
        let r = contrast(
            t.color(Mode::Light, "text.primary").unwrap(),
            t.color(Mode::Light, "bg.panel").unwrap(),
        );
        assert!((r - 15.54).abs() < 0.01, "{r}");
    }

    #[test]
    fn external_zoom_is_adopted() {
        let ctx = egui::Context::default();
        let mut th = Theme::new(Mode::Light, Density::Compact);
        // egui applies a new zoom factor at the end of the pass.
        let pass = |th: &mut Theme, ext: Option<f32>| {
            let _ = ctx.run(Default::default(), |c| {
                if let Some(z) = ext {
                    c.set_zoom_factor(z);
                }
            });
            let _ = ctx.run(Default::default(), |c| {
                th.apply(c);
            });
            let _ = ctx.run(Default::default(), |_| {});
        };
        pass(&mut th, None);
        pass(&mut th, Some(1.25));
        assert_eq!((th.scale, ctx.zoom_factor()), (1.25, 1.25));
        pass(&mut th, Some(3.0));
        assert_eq!((th.scale, ctx.zoom_factor()), (2.0, 2.0));
        th.set_scale(0.75);
        pass(&mut th, None);
        assert_eq!((th.scale, ctx.zoom_factor()), (0.75, 0.75));
    }

    #[test]
    fn init_disables_egui_kb_zoom() {
        let ctx = egui::Context::default();
        assert!(ctx.options(|o| o.zoom_with_keyboard));
        let mut th = Theme::new(Mode::Light, Density::Compact);
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
        assert!(!ctx.options(|o| o.zoom_with_keyboard));
    }

    #[test]
    fn scale_snaps_to_steps() {
        let mut th = Theme::new(Mode::Light, Density::Compact);
        th.set_scale(0.1);
        assert_eq!(th.scale, 0.75);
        th.set_scale(5.0);
        assert_eq!(th.scale, 2.0);
        th.set_scale(1.3);
        assert_eq!(th.scale, 1.25);
    }
}
