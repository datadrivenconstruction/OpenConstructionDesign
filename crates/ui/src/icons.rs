//! Icons: the generated set `docs/design/icons_v4/<direction>/<master>/<name>.svg`
//! (`tools/icons/gen.py`, ICONS_V4.md). Each size draws the optical master that matches its
//! physical pixels (16 / 20 / 24, 1:1), rasterized to a white alpha mask, cached as an egui
//! texture and tinted at draw time (`currentColor` = the caller's ink).
//! The ribbon set `assets/icons3/<master>/<name>.svg` (`tools/icons/ribbon.py`) carries its own
//! role colours: the light `icon.*` token values, swapped for the theme's at rasterization.
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Drawing direction (ICONS_V4.md): A outline, B duotone, C filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Outline,
    Duotone,
    Filled,
    /// Full colour from role tokens (the former default), `assets/icons3`.
    Ribbon,
    /// One ink at three opacities, tinted at draw time (`assets/icons_pick/hierarchical`).
    Hierarchical,
    /// Role-coloured surfaces with an ink edge (`assets/icons_pick/color`).
    Color,
    /// Thin single-weight strokes in role colours (`assets/icons_pick/monoline`).
    Monoline,
    /// Role-coloured glyph on an isometric plate (`assets/icons_pick/isometric`).
    Isometric,
}

impl Direction {
    /// The monochrome directions of `docs/design/icons_v4` (the other sets live apart).
    pub const ALL: [Direction; 3] = [Direction::Outline, Direction::Duotone, Direction::Filled];
    pub fn dir(self) -> &'static str {
        match self {
            Direction::Outline => "outline",
            Direction::Duotone => "duotone",
            Direction::Filled => "filled",
            Direction::Ribbon => "ribbon",
            Direction::Hierarchical => "hierarchical",
            Direction::Color => "color",
            Direction::Monoline => "monoline",
            Direction::Isometric => "isometric",
        }
    }
    /// Files carry their own role colours (swapped for the dark theme at rasterization).
    pub fn is_colour(self) -> bool {
        matches!(
            self,
            Direction::Ribbon | Direction::Color | Direction::Monoline | Direction::Isometric
        )
    }
    /// Directions of the style pack `assets/icons_pick`.
    pub fn is_pick(self) -> bool {
        matches!(
            self,
            Direction::Hierarchical | Direction::Color | Direction::Monoline | Direction::Isometric
        )
    }
}

/// Which direction each use draws, from `data/icon_style.json`: the one switch for the product
/// (the default `duotone_all`: direction B at rest, C for the active tool).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    /// Ribbon, menus, toolbars, panels.
    pub base: Direction,
    /// The selected / active tool (`<name>.fill`).
    pub active: Direction,
    /// Illustrations from `LARGE` logical px up: empty states, onboarding.
    pub large: Direction,
}

pub const STYLE_JSON: &str = include_str!("../data/icon_style.json");

/// One selectable icon set: an id and the direction of each use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconSet {
    pub id: &'static str,
    pub base: Direction,
    pub active: Direction,
    pub large: Direction,
    /// Kept so saved preferences and looks still resolve; not offered by the pickers.
    #[serde(default)]
    pub legacy: bool,
}

impl IconSet {
    pub fn style(&self) -> Style {
        Style {
            base: self.base,
            active: self.active,
            large: self.large,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleFile {
    default: &'static str,
    sets: Vec<IconSet>,
}

fn style_file() -> &'static StyleFile {
    static S: OnceLock<StyleFile> = OnceLock::new();
    S.get_or_init(|| serde_json::from_str(STYLE_JSON).expect("data/icon_style.json"))
}

/// Every icon set the pickers offer (onboarding, settings), in their order.
pub fn sets() -> &'static [IconSet] {
    static V: OnceLock<Vec<IconSet>> = OnceLock::new();
    V.get_or_init(|| style_file().sets.iter().copied().filter(|s| !s.legacy).collect())
}

/// Every set that resolves, the retired ones included.
pub fn all_sets() -> &'static [IconSet] {
    &style_file().sets
}

/// The set with `id`; None for an unknown id.
pub fn set(id: &str) -> Option<&'static IconSet> {
    all_sets().iter().find(|s| s.id == id)
}

/// The product default set.
pub fn style() -> Style {
    set(style_file().default)
        .expect("data/icon_style.json: default names a set")
        .style()
}

/// Optical masters, px.
pub const MASTERS: [u32; 3] = [16, 20, 24];
/// From this logical size up an icon is an illustration and takes `Style::large`.
pub const LARGE: u32 = 32;
/// Accent of the duotone surface and spark in the files: only the layer-split marker; the app
/// paints that layer in `icon.tint` (`icon.tint.ai` for an AI glyph).
const FILE_ACCENT: &str = "#2457D6";

/// An AI glyph (`spark*`, `ai-*`): its accent layer takes `icon.tint.ai`, so violet always means AI.
pub fn is_ai(name: &str) -> bool {
    let n = name.strip_suffix(".fill").unwrap_or(name);
    n.starts_with("spark") || n.starts_with("ai-")
}

/// Colour token of the accent layer of `name`.
pub fn tint_token(name: &str) -> &'static str {
    if is_ai(name) { "icon.tint.ai" } else { "icon.tint" }
}

/// The master drawn for `px` physical pixels: never a fractional shrink of a bigger master
/// (20 px is the 20 master, not the 24 one at 5/6), larger sizes scale the 24 master.
pub fn master_for(px: u32) -> u32 {
    match px {
        0..=19 => 16,
        20..=23 => 20,
        _ => 24,
    }
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| crate::fragments::ICONS_V4.iter().copied().collect())
}

fn ribbon_table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| crate::fragments::ICONS_RIBBON.iter().copied().collect())
}

fn pick_table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| crate::fragments::ICONS_PICK.iter().copied().collect())
}

/// The file `<direction>/<master>/<name>.svg`.
pub fn svg(dir: Direction, master: u32, name: &str) -> Option<&'static str> {
    if dir == Direction::Ribbon {
        return ribbon_table().get(format!("{master}/{name}").as_str()).copied();
    }
    if dir.is_pick() {
        return pick_table()
            .get(format!("{}/{master}/{name}", dir.dir()).as_str())
            .copied();
    }
    table()
        .get(format!("{}/{master}/{name}", dir.dir()).as_str())
        .copied()
}

/// Every glyph name of the set, sorted.
pub fn names() -> &'static [&'static str] {
    static N: OnceLock<Vec<&'static str>> = OnceLock::new();
    N.get_or_init(|| {
        let mut v: Vec<&str> = crate::fragments::ICONS_V4
            .iter()
            .filter_map(|(k, _)| k.strip_prefix("outline/24/"))
            .collect();
        v.sort_unstable();
        v
    })
}

/// Direction and bare name for an icon reference: `<name>.fill` is the active variant.
pub fn resolve(name: &str, logical: u32) -> (Direction, &str) {
    resolve_with(style(), name, logical)
}

pub fn resolve_with(st: Style, name: &str, logical: u32) -> (Direction, &str) {
    match name.strip_suffix(".fill") {
        Some(n) => (st.active, n),
        None if logical >= LARGE => (st.large, name),
        None => (st.base, name),
    }
}
/// The 24 master of an icon reference (`<name>` outline, `<name>.fill` filled); `None` for a
/// name the set does not draw.
pub fn source(name: &str) -> Option<&'static str> {
    match name.strip_suffix(".fill") {
        Some(n) => svg(Direction::Filled, 24, n),
        None => svg(Direction::Outline, 24, name),
    }
}

#[derive(Debug)]
pub enum IconError {
    Unknown(String),
    Parse(String, String),
    Size(u32),
}

/// Which part of a file a texture holds: everything, the ink without the duotone accent, or the
/// accent alone (surface at its 20 % and the spark solid, tinted with the theme accent).
/// `Colour` / `Gray`: a ribbon file in its own colours for the light (`false`) or dark theme,
/// grey for a disabled command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    All,
    Ink,
    Accent,
    Colour(bool),
    Gray(bool),
}

/// Rasterize an icon reference at `px` physical pixels (logical = physical) as one mask.
pub fn rasterize(name: &str, px: u32) -> Result<ColorImage, IconError> {
    let (dir, bare) = resolve(name, px);
    rasterize_as(dir, bare, px, Layer::All)
}

/// Rasterize `name` in `dir` from the master for `px`; an absent accent layer is empty.
pub fn rasterize_as(
    dir: Direction,
    name: &str,
    px: u32,
    layer: Layer,
) -> Result<ColorImage, IconError> {
    let svg = svg(dir, master_for(px), name).ok_or_else(|| IconError::Unknown(name.into()))?;
    let res = match layer {
        Layer::Colour(dark) => rasterize_colour(svg, px, dark),
        Layer::Gray(dark) => rasterize_colour(svg, px, dark).map(grey),
        // the coverage of a ribbon file as a white mask, like every other direction
        Layer::All if dir.is_colour() => rasterize_colour(svg, px, false).map(white),
        _ => rasterize_svg(&split_layer(svg, layer), px),
    };
    res.map_err(|e| match e {
        IconError::Parse(_, m) => IconError::Parse(name.into(), m),
        e => e,
    })
}

/// Keeps the leaf elements of `layer`: accent elements carry the file accent colour. Masks and
/// groups stay in both layers so the knock-outs still apply.
pub fn split_layer(svg: &str, layer: Layer) -> String {
    if layer == Layer::All {
        return svg.to_string();
    }
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    let mut in_mask = 0i32;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            break;
        };
        let tag = &rest[i..i + j + 1];
        rest = &rest[i + j + 1..];
        let leaf = tag.ends_with("/>");
        if tag.starts_with("<mask") {
            in_mask += 1;
        } else if tag.starts_with("</mask") {
            in_mask -= 1;
        }
        let acc = tag.contains(FILE_ACCENT);
        let keep = !leaf || in_mask > 0 || (acc == (layer == Layer::Accent));
        if keep {
            out.push_str(tag);
        }
    }
    out.push_str(rest);
    out
}

/// `(light, theme)` values of the `icon.*` role tokens: a ribbon file is drawn in the light
/// values, the dark theme swaps each for its own.
pub fn role_colours(dark: bool) -> &'static [(String, String)] {
    static P: OnceLock<[Vec<(String, String)>; 2]> = OnceLock::new();
    let p = P.get_or_init(|| {
        use crate::theme::{Mode, Tokens};
        let t = Tokens::embedded().expect("embedded tokens.json is validated by tests");
        let of = |m| {
            t.palette(Mode::Light)
                .iter()
                .filter(|(k, _)| k.starts_with("icon."))
                .map(|(k, v)| (v.clone(), t.palette(m)[k].clone()))
                .collect::<Vec<_>>()
        };
        [of(Mode::Light), of(Mode::Dark)]
    });
    &p[usize::from(dark)]
}

/// A ribbon glyph without role colours (controls): drawn as a mask in the caller's ink.
pub fn is_mono(svg: &str) -> bool {
    svg.contains("currentColor")
}

/// A ribbon file in the theme's role colours at `px` (premultiplied RGBA).
pub fn rasterize_colour(svg: &str, px: u32, dark: bool) -> Result<ColorImage, IconError> {
    let mut s = svg.to_string();
    if dark {
        for (light, theme) in role_colours(true) {
            s = s.replace(light.as_str(), theme);
        }
    }
    render_svg(&s.replace("currentColor", "#FFFFFF"), px)
}

/// Coverage only: every pixel white at its own alpha.
fn white(mut img: ColorImage) -> ColorImage {
    for p in &mut img.pixels {
        let a = p.a();
        *p = Color32::from_rgba_premultiplied(a, a, a, a);
    }
    img
}

/// Luminance only: the disabled look of a coloured glyph.
fn grey(mut img: ColorImage) -> ColorImage {
    for p in &mut img.pixels {
        let l = 0.3 * p.r() as f32 + 0.59 * p.g() as f32 + 0.11 * p.b() as f32;
        let l = l.round().min(p.a() as f32) as u8;
        *p = Color32::from_rgba_premultiplied(l, l, l, p.a());
    }
    img
}

/// White mask of `svg` scaled to `px` (1:1 when `px` is the file's master).
pub fn rasterize_svg(svg: &str, px: u32) -> Result<ColorImage, IconError> {
    let svg = svg
        .replace("currentColor", "#FFFFFF")
        .replace(FILE_ACCENT, "#FFFFFF");
    render_svg(&svg, px)
}

fn render_svg(svg: &str, px: u32) -> Result<ColorImage, IconError> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .map_err(|e| IconError::Parse(String::new(), e.to_string()))?;
    let mut pm = tiny_skia::Pixmap::new(px, px).ok_or(IconError::Size(px))?;
    let s = px as f32 / tree.size().width().max(1.0);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(s, s),
        &mut pm.as_mut(),
    );
    Ok(ColorImage::from_rgba_premultiplied(
        [px as usize, px as usize],
        pm.data(),
    ))
}

/// Icon ink by state (ICONS_V4.md colour roles), from theme tokens so the dark theme follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    /// At rest and on hover (the hover shows in the tile background).
    Rest,
    /// The selected / active tool.
    Active,
    /// A command that cannot run now.
    Disabled,
}

/// Opacity of a disabled icon.
pub const DISABLED_INK: f32 = 0.4;
/// Ink alpha under which a coloured ribbon glyph paints grey (a disabled command).
pub const DISABLED_BELOW: u8 = 128;

pub fn tint(th: &crate::Theme, t: Tint) -> Color32 {
    match t {
        Tint::Rest => th.c("text.primary"),
        Tint::Active => th.c("icon.tint"),
        Tint::Disabled => th.c("text.primary").gamma_multiply(DISABLED_INK),
    }
}

/// Texture cache keyed by (icon reference, physical px, layer). Unknown icons get an outlined
/// placeholder.
#[derive(Default)]
pub struct IconCache {
    tex: HashMap<(String, u32, Layer), Option<TextureHandle>>,
    /// The person's set; None draws the product default.
    style: Option<Style>,
}

impl IconCache {
    /// Draw with `st` from now on; a change drops the textures of the previous set.
    pub fn set_style(&mut self, st: Style) {
        if self.style() != st {
            self.tex.clear();
        }
        self.style = Some(st);
    }
    pub fn style(&self) -> Style {
        self.style.unwrap_or_else(style)
    }
    fn resolve<'a>(&self, name: &'a str, logical: u32) -> (Direction, &'a str) {
        resolve_with(self.style(), name, logical)
    }
    /// `logical` size × `ppp` (pixels per point) → texture of the whole file at exact physical px.
    pub fn get(&mut self, ctx: &egui::Context, name: &str, logical: u32) -> TextureHandle {
        let (dir, bare) = self.resolve(name, logical);
        self.layer(ctx, name, dir, bare, logical, Layer::All)
            .expect("the whole file always has a texture (placeholder when unknown)")
    }

    fn layer(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        dir: Direction,
        bare: &str,
        logical: u32,
        layer: Layer,
    ) -> Option<TextureHandle> {
        let px = ((logical as f32) * ctx.pixels_per_point()).round().max(1.0) as u32;
        let key = (format!("{name}@{logical}"), px, layer);
        self.tex
            .entry(key)
            .or_insert_with(|| {
                let img = match rasterize_as(dir, bare, px, layer) {
                    Ok(img) if layer == Layer::Accent && img.pixels.iter().all(|p| p.a() == 0) => {
                        return None;
                    }
                    Ok(img) => img,
                    Err(_) if layer == Layer::Accent => return None,
                    Err(_) => placeholder(px),
                };
                let id = format!("icon:{name}@{px}:{}:{layer:?}", dir.dir());
                Some(ctx.load_texture(id, img, TextureOptions::LINEAR))
            })
            .clone()
    }
    pub fn len(&self) -> usize {
        self.tex.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tex.is_empty()
    }
    /// Paint an icon tinted with `color` into `rect`; a duotone file paints its accent layer in
    /// the theme accent on top, faded with `color`.
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        name: &str,
        logical: u32,
        rect: egui::Rect,
        color: Color32,
    ) {
        // align to the physical pixel grid: a texture drawn at a fractional offset blurs
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let rect =
            egui::Rect::from_min_size(egui::pos2(snap(rect.min.x), snap(rect.min.y)), rect.size());
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let ctx = ui.ctx().clone();
        let (dir, bare) = self.resolve(name, logical);
        if dir.is_colour() && svg(dir, master_for(logical), bare).is_some_and(|s| !is_mono(s)) {
            // own colours: the caller's ink only fades them; a faint ink is a disabled command
            let dark = crate::theme::of(&ctx).mode == crate::theme::Mode::Dark;
            let off = color.a() < DISABLED_BELOW;
            let layer = if off { Layer::Gray(dark) } else { Layer::Colour(dark) };
            let fade = if off { color.a() as f32 / 255.0 } else { 1.0 };
            if let Some(t) = self.layer(&ctx, name, dir, bare, logical, layer) {
                ui.painter().image(t.id(), rect, uv, Color32::WHITE.gamma_multiply(fade));
            }
            return;
        }
        if dir != Direction::Duotone {
            let t = self.get(&ctx, name, logical);
            ui.painter().image(t.id(), rect, uv, color);
            return;
        }
        if let Some(t) = self.layer(&ctx, name, dir, bare, logical, Layer::Ink) {
            ui.painter().image(t.id(), rect, uv, color);
        }
        if let Some(t) = self.layer(&ctx, name, dir, bare, logical, Layer::Accent) {
            let fade = color.a() as f32 / 255.0;
            let accent = crate::theme::of(&ctx).c(tint_token(bare)).gamma_multiply(fade);
            ui.painter().image(t.id(), rect, uv, accent);
        }
    }
}

/// Control glyphs for widgets that used to stroke chevrons / checks / sort arrows by hand
/// (`field.rs`, `menu.rs`, `table.rs`, `choice.rs`): paint them with `Icons::paint(ui, name, 16, ..)`.
pub mod ctl {
    pub const CHEVRON_DOWN: &str = "chevron-down";
    pub const CHEVRON_UP: &str = "chevron-up";
    pub const CHEVRON_LEFT: &str = "chevron-left";
    pub const CHEVRON_RIGHT: &str = "chevron-right";
    pub const CHECK: &str = "check-mark";
    pub const SORT_ASC: &str = "sort-asc";
    pub const SORT_DESC: &str = "sort-desc";
    /// The one AI glyph (assistant, chat, panel, palette rows).
    pub const AI: &str = "spark";
}

/// Dashed-looking square outline: visible in QA, never a panic.
fn placeholder(px: u32) -> ColorImage {
    let n = px as usize;
    let mut img = ColorImage::filled([n, n], Color32::TRANSPARENT);
    let (a, b) = (n / 6, n - 1 - n / 6);
    for i in a..=b {
        if (i / 2) % 2 == 0 {
            for (x, y) in [(i, a), (i, b), (a, i), (b, i)] {
                img[(x, y)] = Color32::WHITE;
            }
        }
    }
    img
}

#[cfg(test)]
#[path = "icons_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "icons_ribbon_tests.rs"]
mod ribbon_tests;

#[cfg(test)]
#[path = "icons_pick_tests.rs"]
mod pick_tests;

