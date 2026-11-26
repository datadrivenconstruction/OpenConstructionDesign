// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Browser entry (wasm32, feature `web`): the same `EditorApp` on a page canvas, plus the calls the
//! host page (the ERP) uses to load and save the model as `.ddc` bytes. See docs/BROWSER.md.
//! Scaffold: it builds once steps S1-S5 of that doc have landed (`Model::from_bytes`/`to_bytes`,
//! `link::gen::build` outside the `link` feature, rusqlite on wasm32). Desktop never compiles it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use ddc_ai::host_bridge::{Mailbox, BRIDGE};
use ddc_geom::v2;
use ddc_model::Model;
use eframe::egui;
use wasm_bindgen::prelude::*;

use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;

/// Generator brief of the example offered on an empty start (Germany, residential).
const EXAMPLE_DE: &str = include_str!("../../io/contract/examples/brief.de-residential.json");

#[derive(Default)]
struct Host {
    on_change: Option<js_sys::Function>,
    /// `host.ai(request_json) -> Promise<reply_json>`, wire `ddc-web-bridge/1`.
    ai: Option<js_sys::Function>,
    view_only: bool,
    seen: u64,
    first_frame_ms: Option<f64>,
    /// To wake a frame after a call from the page (the runner does not hand it out).
    ctx: Option<egui::Context>,
    /// `cb(keys_json)` when the selection changed (bridge `select`).
    on_select: Option<js_sys::Function>,
    /// The selection last told to the host (or set by it: no echo).
    sel: Vec<Sel>,
    /// `ifc_class` and `type_key` per key: the tip's rows, then the last saved draft's.
    info: crate::link::pick::RowInfo,
    /// The chain tip a draft diffs against: its revision id and element rows.
    #[cfg(feature = "link")]
    parent: Option<(String, Vec<ddc_io::elements_parquet::ElementRow>)>,
    /// `cb(paths_json)`: bundle files the UI needs now (locale catalogs, script fonts).
    on_assets: Option<js_sys::Function>,
}

static AI_MAILBOX: OnceLock<Arc<Mailbox>> = OnceLock::new();

/// Bundle paths `ddc_ui::assets` asked for, sent to the page after the frame.
static ASSET_ASKS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn take_asset_asks() -> Vec<String> {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        ddc_ui::assets::set_hook(|p| ASSET_ASKS.lock().unwrap_or_else(|e| e.into_inner()).push(p.to_owned()))
    });
    std::mem::take(&mut *ASSET_ASKS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The page's AI mailbox: a `HostProvider` over it reaches the host's `ai` callback.
pub fn ai_mailbox() -> Arc<Mailbox> {
    AI_MAILBOX.get_or_init(Default::default).clone()
}

fn bridge_error(msg: &str) -> serde_json::Value {
    serde_json::json!({ "bridge": BRIDGE, "error": { "kind": "network", "message": msg } })
}

fn js_text(e: &JsValue) -> String {
    e.as_string().unwrap_or_else(|| "the host refused the AI request".into())
}

/// Hands one envelope to the host; its promise answers into the mailbox and wakes a frame.
fn post_ai(f: &js_sys::Function, ctx: &egui::Context, id: u64, req: serde_json::Value) {
    let mb = ai_mailbox();
    let promise = match f.call1(&JsValue::NULL, &JsValue::from_str(&req.to_string())) {
        Ok(p) => p,
        Err(e) => return mb.answer(id, bridge_error(&js_text(&e))),
    };
    let ctx = ctx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let reply = match wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&promise)).await {
            Ok(v) => v
                .as_string()
                .or_else(|| js_sys::JSON::stringify(&v).ok().map(String::from))
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| bridge_error("the host reply is not JSON")),
            Err(e) => bridge_error(&js_text(&e)),
        };
        mb.answer(id, reply);
        ctx.request_repaint();
    });
}

struct WebApp {
    app: EditorApp,
    host: Rc<RefCell<Host>>,
}

impl eframe::App for WebApp {
    // WebRunner::app_mut downcasts through this; without it every WebEditor call panics
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.ui(ctx, frame.wgpu_render_state());
        let rev = self.app.ed.revision;
        let mut h = self.host.borrow_mut();
        if h.first_frame_ms.is_none() {
            h.first_frame_ms = Some(now_ms());
        }
        if let Some(f) = h.ai.clone() {
            for (id, req) in ai_mailbox().take_outbox() {
                post_ai(&f, ctx, id, req);
            }
        }
        let picked = (self.app.ed.selection != h.sel).then(|| {
            h.sel = self.app.ed.selection.clone();
            let f = h.on_select.clone()?;
            Some((f, serde_json::json!(crate::link::pick::selection_elements(&self.app.ed, &h.info)).to_string()))
        });
        let changed = (rev != h.seen).then(|| {
            h.seen = rev;
            h.on_change.clone()
        });
        // kept queued until the page listens
        let assets = h.on_assets.clone().map(|f| (f, take_asset_asks())).filter(|(_, p)| !p.is_empty());
        // the callbacks may call back into the editor: no borrow held
        drop(h);
        if let Some((f, paths)) = assets {
            let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&serde_json::json!(paths).to_string()));
        }
        if let Some(Some((f, elements))) = picked {
            let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&elements));
        }
        if let Some(Some(f)) = changed {
            let _ = f.call1(&JsValue::NULL, &JsValue::from_f64(rev as f64));
        }
    }
}

fn now_ms() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or(0.0)
}

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Editor built from the DE example brief, in memory.
pub fn example_editor() -> Result<Editor, String> {
    let brief: serde_json::Value = serde_json::from_str(EXAMPLE_DE).map_err(|e| e.to_string())?;
    let (ed, _) = crate::link::gen::build(&brief).map_err(|e| e.to_string())?;
    Ok(ed)
}

/// The editor on one canvas. JS: `const ed = new WebEditor(); await ed.start("canvas-id")`.
#[wasm_bindgen]
pub struct WebEditor {
    runner: eframe::WebRunner,
    host: Rc<RefCell<Host>>,
}

#[wasm_bindgen]
impl WebEditor {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WebEditor {
        WebEditor { runner: eframe::WebRunner::new(), host: Default::default() }
    }

    /// Starts on the canvas `canvas_id` with an empty model or the DE example (`example`).
    /// `webgpu` false keeps to WebGL2 (works everywhere, also headless).
    pub async fn start(&self, canvas_id: &str, example: bool, webgpu: bool) -> Result<(), JsValue> {
        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(canvas_id))
            .ok_or_else(|| err(format!("no element #{canvas_id}")))?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| err(format!("#{canvas_id} is not a canvas")))?;
        let backends = if webgpu { wgpu::Backends::BROWSER_WEBGPU } else { wgpu::Backends::GL };
        let opts = eframe::WebOptions {
            wgpu_options: crate::gfx::wgpu_options(crate::gfx::Pick { backends, software: false }),
            ..Default::default()
        };
        let host = self.host.clone();
        self.runner
            .start(
                canvas,
                opts,
                Box::new(move |cc| {
                    let ed = if example { example_editor()? } else { Editor::new_in_memory()? };
                    let rs = cc.wgpu_render_state.as_ref();
                    let backend = rs.map(|r| format!("{:?}", r.adapter.get_info().backend)).unwrap_or_default();
                    let app = EditorApp::new(ed, rs, backend);
                    {
                        let mut h = host.borrow_mut();
                        h.seen = app.ed.revision;
                        h.ctx = Some(cc.egui_ctx.clone());
                    }
                    Ok(Box::new(WebApp { app, host }))
                }),
            )
            .await
    }

    fn with<R>(&self, f: impl FnOnce(&mut EditorApp) -> R) -> Result<R, JsValue> {
        let mut w = self.runner.app_mut::<WebApp>().ok_or_else(|| err("editor not started"))?;
        let r = f(&mut w.app);
        drop(w);
        if let Some(ctx) = self.host.borrow().ctx.clone() {
            ctx.request_repaint();
        }
        Ok(r)
    }

    /// Shows `m`; a view-only page locks it once loaded (the load itself may write caches).
    fn adopt(&self, m: Model) -> Result<(), JsValue> {
        let view_only = self.host.borrow().view_only;
        self.with(|a| {
            a.ed.replace_model(m)?;
            a.ed.model.set_query_only(view_only)
        })?
        .map_err(err)
    }

    fn editable(&self) -> Result<(), JsValue> {
        if self.host.borrow().view_only {
            return Err(err("view only: edit in the editor on a computer"));
        }
        Ok(())
    }

    /// View-only (tablet, phone): navigation, levels and selection work, every model write is
    /// refused by the database. The host turns it on for touch and small screens.
    pub fn set_view_only(&self, on: bool) -> Result<(), JsValue> {
        self.host.borrow_mut().view_only = on;
        self.with(|a| {
            a.ed.selection.clear();
            a.ed.model.set_query_only(on)
        })?
        .map_err(err)
    }

    pub fn view_only(&self) -> bool {
        self.host.borrow().view_only
    }

    /// Replaces the model with a `.ddc` file (bytes from the host); history and selection reset.
    pub fn load_model(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let m = Model::from_bytes(bytes).map_err(err)?;
        self.adopt(m)
    }

    /// The model as `.ddc` file bytes, for the host to store as a revision.
    pub fn save_model(&self) -> Result<Vec<u8>, JsValue> {
        self.with(|a| a.ed.model.to_bytes())?.map_err(err)
    }

    /// Replaces the model with the generated example (`de` only for now).
    pub fn generate_example(&self, country: &str) -> Result<(), JsValue> {
        if country != "de" {
            return Err(err(format!("no example for {country}")));
        }
        let ed = example_editor().map_err(err)?;
        self.adopt(ed.model)
    }

    /// Model revision: changes with every edit (the host saves when it moves).
    pub fn revision(&self) -> Result<f64, JsValue> {
        self.with(|a| a.ed.revision as f64)
    }

    /// Stops the editor on its canvas (the host unmounts it); the canvas stays to the host.
    pub fn destroy(&self) {
        self.runner.destroy();
    }

    /// `cb(revision)` after every frame that changed the model.
    pub fn on_change(&self, cb: js_sys::Function) {
        self.host.borrow_mut().on_change = Some(cb);
    }

    /// `cb(elements_json)` after a frame that changed the selection: the bridge `select` elements,
    /// `[{"element_key", "ifc_class"?, "type_key"?}, ...]`. Called inside the frame: the host
    /// answers later (a task), not from the callback.
    pub fn on_select(&self, cb: js_sys::Function) {
        self.host.borrow_mut().on_select = Some(cb);
    }

    /// Element keys of the selection, JSON.
    pub fn selection_keys(&self) -> Result<String, JsValue> {
        self.with(|a| serde_json::json!(crate::link::pick::keys_of_selection(&a.ed)).to_string())
    }

    /// Selects the elements of `keys_json` (the host's `select`); returns how many were found.
    /// Not echoed back through `on_select`.
    pub fn select_keys(&self, keys_json: &str) -> Result<u32, JsValue> {
        let keys: Vec<String> = serde_json::from_str(keys_json).map_err(err)?;
        let (n, sel) = self.with(|a| crate::link::pick::select_keys(&mut a.ed, &keys).map(|n| (n, a.ed.selection.clone())))?.map_err(err)?;
        self.host.borrow_mut().sel = sel;
        Ok(n as u32)
    }

    /// The assistant tool call `{name, args}` that drafts a brief confirmed in the ERP as a plan
    /// item (preview, then Apply), JSON; the page hands it to the AI panel as its model's answer.
    pub fn brief_tool_call(&self, brief_json: &str) -> Result<String, JsValue> {
        let brief: serde_json::Value = serde_json::from_str(brief_json).map_err(err)?;
        self.with(|a| crate::link::gen::brief_tool_call(&a.ed.model, &brief).map(|c| c.to_string()))?.map_err(err)
    }

    /// UI language by locale code (`de`, `ru-RU`; unknown gives English). A language other than
    /// en/ru shows English until its bundle files arrive (`on_assets`, `add_asset`).
    pub fn set_locale(&self, code: &str) -> Result<(), JsValue> {
        let lang = ddc_ui::i18n::Lang::from_code(code);
        self.with(|a| a.set_lang(lang))
    }

    /// `cb(paths_json)` after a frame that needed bundle files not loaded yet (`i18n/<code>.json`,
    /// `fonts/<file>`); the page fetches each, checks it against the manifest and calls `add_asset`.
    /// Paths asked before the callback was set come with the next frame.
    pub fn on_assets(&self, cb: js_sys::Function) {
        self.host.borrow_mut().on_assets = Some(cb);
        if let Some(ctx) = self.host.borrow().ctx.clone() {
            ctx.request_repaint();
        }
    }

    /// A bundle file asked for through `on_assets`; the language is applied again so every text
    /// built from the catalog picks it up.
    pub fn add_asset(&self, path: &str, bytes: &[u8]) -> Result<(), JsValue> {
        let ok = |dir: &str, ext: &str| {
            path.strip_prefix(dir).and_then(|n| n.strip_suffix(ext)).is_some_and(|n| {
                !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        };
        if !ok("i18n/", ".json") && !ok("fonts/", ".ttf") {
            return Err(err(format!("{path}: not a bundle asset")));
        }
        ddc_ui::assets::add(path, bytes.to_vec());
        self.with(|a| a.set_lang(a.sh.cat.lang))
    }

    /// `light` or `dark`.
    pub fn set_theme(&self, theme: &str) -> Result<(), JsValue> {
        if theme != "light" && theme != "dark" {
            return Err(err(format!("theme '{theme}': light or dark")));
        }
        self.with(|a| a.prefs.theme = theme.into())
    }

    /// The host's AI: `cb(request_json) -> Promise<reply_json>` (wire `ddc-web-bridge/1`). The host
    /// answers with the project's ERP AI profile; no key reaches the page.
    pub fn set_ai_host(&self, cb: js_sys::Function) {
        self.host.borrow_mut().ai = Some(cb);
        // a frame to post what waits (before `start` there is none to wake)
        let _ = self.with(|_| ());
    }

    /// AI requests posted to the host and not yet taken back with their reply.
    pub fn ai_open(&self) -> f64 {
        ai_mailbox().open() as f64
    }

    /// `performance.now()` of the first painted frame, or -1 before it.
    pub fn first_frame_ms(&self) -> f64 {
        self.host.borrow().first_frame_ms.unwrap_or(-1.0)
    }

    /// Element counts by kind (walls, slabs, ...), JSON.
    pub fn counts(&self) -> Result<String, JsValue> {
        self.with(|a| {
            let c: serde_json::Map<String, serde_json::Value> = crate::headless_file::counts(&a.ed.model)
                .into_iter()
                .map(|(k, n)| (k.to_string(), n.into()))
                .collect();
            serde_json::Value::Object(c).to_string()
        })
    }

    /// Levels as JSON `[[id, name, elevation], ...]` and the shown one.
    pub fn levels(&self) -> Result<String, JsValue> {
        self.with(|a| serde_json::json!({"levels": a.ed.levels(), "current": a.ed.level}).to_string())
    }

    /// Shows level `id` (what the level switcher does).
    pub fn set_level(&self, id: f64) -> Result<(), JsValue> {
        self.with(|a| a.ed.set_level(id as i64))
    }

    /// Selects wall number `i` of the shown level; returns its id or -1.
    pub fn select_wall(&self, i: u32) -> Result<f64, JsValue> {
        self.with(|a| match a.ed.walls.get(i as usize).map(|w| w.id) {
            Some(id) => {
                a.ed.selection = vec![Sel::Wall(id)];
                id as f64
            }
            None => -1.0,
        })
    }

    /// Ids of the selected walls, JSON.
    pub fn selection(&self) -> Result<String, JsValue> {
        self.with(|a| {
            let ids: Vec<i64> =
                a.ed.selection.iter().filter_map(|s| if let Sel::Wall(id) = s { Some(*id) } else { None }).collect();
            serde_json::json!(ids).to_string()
        })
    }

    /// Moves the selection by (dx, dy) metres with the Move tool (two clicks).
    pub fn move_selection(&self, dx: f64, dy: f64) -> Result<String, JsValue> {
        self.editable()?;
        self.with(|a| {
            use crate::modify_tools::{ModTool, Op, Out};
            let sel = a.ed.selection.clone();
            let mut t = ModTool::new(Op::Move, &sel);
            let _ = t.click(&mut a.ed, v2(0.0, 0.0));
            match t.click(&mut a.ed, v2(dx, dy)) {
                Out::Done | Out::Exit => String::new(),
                Out::Refused(m) | Out::Error(m) => m,
                Out::Nothing => "nothing moved".into(),
            }
        })
    }

    /// Deletes the selection (as the Delete key).
    pub fn delete_selection(&self) -> Result<(), JsValue> {
        self.editable()?;
        self.with(|a| a.ed.delete_selection())
    }

    /// Wall start/end of wall `id`, JSON `[x0, y0, x1, y1]`, or null.
    pub fn wall_line(&self, id: f64) -> Result<String, JsValue> {
        self.with(|a| {
            let w = a.ed.walls.iter().find(|w| w.id == id as i64);
            match w {
                Some(w) => serde_json::json!([w.a.x, w.a.y, w.b.x, w.b.y]).to_string(),
                None => "null".into(),
            }
        })
    }
}

/// Drafts as `.ddcrev` packages (contract `ddc-revision/1`), built and read in the page.
#[cfg(feature = "link")]
#[wasm_bindgen]
impl WebEditor {
    /// Replaces the model with the `model.ddc` of a `.ddcrev` (the person's draft); the package
    /// is checked against its own manifest first.
    pub fn load_package(&self, bytes: &[u8]) -> Result<(), JsValue> {
        use crate::link::package;
        package::verify_bytes(bytes).map_err(err)?;
        let files = package::read_all_bytes(bytes).map_err(err)?;
        let model = files.get("model.ddc").ok_or_else(|| err("the package has no model.ddc"))?;
        self.load_model(model)
    }

    /// The chain tip a draft builds on, from its `elements.parquet`.
    pub fn set_parent_rows(&self, revision_id: &str, parquet: &[u8]) -> Result<(), JsValue> {
        let rows = ddc_io::elements_parquet::read_parquet(parquet.to_vec()).map_err(err)?;
        self.set_parent(revision_id, rows);
        Ok(())
    }

    /// The chain tip a draft builds on, from its `model.ddc` (rows computed here).
    pub fn set_parent_model(&self, revision_id: &str, ddc: &[u8]) -> Result<(), JsValue> {
        let rows = crate::link::publish::rows_of_model(ddc).map_err(|e| err(e.msg))?;
        self.set_parent(revision_id, rows);
        Ok(())
    }
    /// No chain tip: the next draft is the project's first design.
    pub fn clear_parent(&self) {
        self.host.borrow_mut().parent = None;
    }

    /// The open model as the draft `.ddcrev` for the host to store (`save`). `draft_id` keeps the
    /// draft's revision id across saves; minted element keys stay in the model.
    pub fn save_package(&self, project_id: &str, name: &str, draft_id: Option<String>, depth: Option<String>) -> Result<Vec<u8>, JsValue> {
        self.editable()?;
        let parent = self.host.borrow().parent.clone();
        let d = crate::link::publish::DraftArgs {
            project_id: project_id.into(),
            name: name.into(),
            revision_id: draft_id,
            parent,
            depth,
            author: None,
        };
        let (zip, rows) = self.with(|a| crate::link::publish::package_draft(&mut a.ed, d))?.map_err(|e| {
            let more = e.details.map(|d| format!(": {d}")).unwrap_or_default();
            err(format!("{}: {}{more}", e.kind, e.msg))
        })?;
        remember_rows(&mut self.host.borrow_mut().info, &rows);
        Ok(zip)
    }
}

#[cfg(feature = "link")]
impl WebEditor {
    fn set_parent(&self, revision_id: &str, rows: Vec<ddc_io::elements_parquet::ElementRow>) {
        let mut h = self.host.borrow_mut();
        remember_rows(&mut h.info, &rows);
        h.parent = Some((revision_id.to_string(), rows));
    }
}

#[cfg(feature = "link")]
fn remember_rows(info: &mut crate::link::pick::RowInfo, rows: &[ddc_io::elements_parquet::ElementRow]) {
    for r in rows {
        info.insert(r.key.clone(), (r.ifc_class.clone(), r.type_key.clone()));
    }
}

impl Default for WebEditor {
    fn default() -> Self {
        Self::new()
    }
}

/// Panics go to the browser console (once per page).
#[wasm_bindgen(start)]
pub fn web_main() {
    std::panic::set_hook(Box::new(|p| {
        web_sys::console::error_1(&JsValue::from_str(&p.to_string()));
    }));
}
