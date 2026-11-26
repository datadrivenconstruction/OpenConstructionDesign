// Bundle entry `ddc-editor.js` (manifest ddc-web-editor/1): the ERP imports this module and calls
// mount(element, host); every message both ways is ddc-web-bridge/1 (contract README section 11).
// The page never fetches anything but its own bundle files (start files, then on-demand locale
// catalogs and fonts): models come through `read_file`, drafts go out through `save`, AI through
// `ai_request`; the session token stays with the host.
import init, { WebEditor } from "./ddc_web_editor.js";

const BRIDGE = 1;
const NAME = "design editor (browser)";
const VERSION = "__VERSION__"; // build_bundle.sh writes the workspace version here
const DRAFT_FILE = "draft.ddcrev";
const AUTOSAVE_MS = 1500; // idle time after the last edit
const RETRY_MS = 10000;
const MAX_PACKAGE = 64 * 1024 * 1024;
const NARROW_PX = 900;
// the editor's AI panel speaks this wire to its provider; the ERP answers it with brief_from_text
const AI_WIRE = "ddc-web-bridge/1";
// save errors that another try does not cure
const CALL_PREFIX = "erp-brief-";
const FINAL = new Set(["no_permission", "too_large", "rejected", "integrity", "version"]);

let wasmReady = null;
let seq = 0;

function bytesOf(b) {
  if (b instanceof Uint8Array) return b;
  if (b instanceof ArrayBuffer) return new Uint8Array(b);
  const raw = atob(b); // a JSON carrier sends base64
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

async function sha256Hex(bytes) {
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(d)].map((x) => x.toString(16).padStart(2, "0")).join("");
}

function newId() {
  if (crypto.randomUUID) return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Phones, tablets and narrow windows only look and select (the host decides too; both must agree). */
function smallDevice() {
  const coarse = typeof matchMedia === "function" && !matchMedia("(pointer: fine)").matches;
  return window.innerWidth < NARROW_PX || coarse;
}

function lastUserText(req) {
  for (let i = (req.messages || []).length - 1; i >= 0; i--) {
    const m = req.messages[i];
    if (m.role !== "user") continue;
    const t = (m.parts || []).filter((p) => p.type === "text").map((p) => p.text).join("\n").trim();
    if (t) return t.slice(0, 20000);
  }
  return "";
}

/** The tool result the AI panel sends back after the drafting call this page made, if it is one. */
function draftResult(req) {
  const last = (req.messages || []).at(-1);
  return (last?.parts || []).find((p) => p.type === "tool_result" && String(p.id).startsWith(CALL_PREFIX));
}

/** ai_result -> the AI panel's provider reply. A confirmed brief comes back as the tool call that
 * drafts it (the desktop assistant's path: plan item, preview, Apply); `call` is that call or an error. */
function aiReply(m, call, tools) {
  const n = (m.confirmed || []).length;
  switch (m.status) {
    case "confirmed": {
      const said = `The ERP saved ${n} confirmed brief value(s) as brief version ${m.brief_version ?? "?"}.`;
      if (call?.name && tools.has(call.name)) {
        const id = `${CALL_PREFIX}${m.brief_version ?? 0}-${Date.now()}`;
        return { bridge: AI_WIRE, text: `${said} Drafting it for review.`, tool_calls: [{ id, name: call.name, args: call.args }], finish: "tool_calls" };
      }
      const why = call?.error ? ` It was not drafted: ${call.error}` : "";
      return { bridge: AI_WIRE, text: said + why, tool_calls: [], finish: "stop" };
    }
    case "declined":
      return { bridge: AI_WIRE, text: "No value was confirmed in the ERP; the brief is unchanged.", tool_calls: [], finish: "stop" };
    case "unavailable":
      return { bridge: AI_WIRE, error: { kind: "blocked", message: "No AI is set up for this project in the ERP." } };
    default:
      return { bridge: AI_WIRE, error: { kind: "network", message: m.error?.message || "the ERP could not run the request" } };
  }
}

export async function mount(element, host) {
  const post = (msg, transfer) => host.post({ bridge: BRIDGE, ...msg }, transfer);
  // first message, before the wasm loads: the host may refuse at once
  post({ type: "ready", versions: [BRIDGE], editor: { name: NAME, version: VERSION } });
  wasmReady ??= init({ module_or_path: new URL("./ddc_web_editor_bg.wasm", import.meta.url) });
  await wasmReady;

  const canvas = document.createElement("canvas");
  canvas.id = `ddc-editor-${++seq}`;
  canvas.style.cssText = "display:block;width:100%;height:100%;outline:none";
  element.appendChild(canvas);
  const ed = new WebEditor();
  await ed.start(canvas.id, false, false);
  ed.set_view_only(true); // until `open` says edit

  const st = {
    open: null,
    refused: false,
    gone: false,
    loading: 0, // generation of the model load in progress; edits during a load are not the person's
    dirty: false,
    quiet: -1, // revision the editor itself made (keys minted by a package): not an edit
    draftId: null,
    saving: false,
    timer: 0,
    req: 0,
    waiting: new Map(), // request_id -> resolve
  };

  const ask = (msg, transfer) =>
    new Promise((resolve) => {
      const request_id = `r${++st.req}`;
      st.waiting.set(request_id, resolve);
      post({ ...msg, request_id }, transfer);
    });

  const editable = () => !!st.open && !st.refused && st.open.mode === "edit" && !!st.open.permissions?.can_edit && !smallDevice();

  // locale catalogs and script fonts come from this bundle when the language needs them; each is
  // checked against the bundle's manifest. A file that does not arrive leaves English / Latin faces.
  const manifest = fetch(new URL("./manifest.json", import.meta.url), { credentials: "omit" })
    .then((r) => (r.ok ? r.json() : null))
    .catch(() => null);
  async function loadAsset(path) {
    const f = (await manifest)?.files?.find((x) => x.path === path && x.load === "on_demand");
    if (!f) return;
    try {
      const r = await fetch(new URL(`./${path}`, import.meta.url), { credentials: "omit" });
      if (!r.ok) return;
      const bytes = new Uint8Array(await r.arrayBuffer());
      if (st.gone || bytes.length !== f.bytes || (await sha256Hex(bytes)) !== f.sha256) return;
      ed.add_asset(path, bytes);
    } catch {
      // offline or unmounted: the fallback stays
    }
  }
  ed.on_assets((json) => JSON.parse(json).forEach(loadAsset));

  function setDirty(on) {
    if (st.dirty === on) return;
    st.dirty = on;
    post({ type: "dirty", dirty: on });
  }

  function schedule(ms) {
    clearTimeout(st.timer);
    st.timer = setTimeout(save, ms);
  }

  async function save() {
    if (st.gone || !st.dirty || !editable()) return;
    if (st.saving) return schedule(AUTOSAVE_MS);
    st.saving = true;
    try {
      const o = st.open;
      const rev = ed.revision();
      let pkg;
      try {
        pkg = ed.save_package(o.project.id, o.project.name || "draft", st.draftId, o.brief?.depth_level ?? undefined);
      } catch (e) {
        console.error("ddc editor: the draft package was not built", e);
        return;
      }
      st.quiet = ed.revision();
      if (pkg.length > MAX_PACKAGE) {
        console.error(`ddc editor: draft of ${pkg.length} bytes is over the 64 MiB limit`);
        return;
      }
      const package_sha256 = await sha256Hex(pkg);
      const r = await ask({ type: "save", package: pkg, package_bytes: pkg.length, package_sha256 }, [pkg.buffer]);
      if (st.gone) return;
      if (r.ok) {
        if (r.draft?.revision_id) st.draftId = r.draft.revision_id;
        // an edit while the save was out keeps it dirty and saves again
        if (ed.revision() === rev || ed.revision() === st.quiet) setDirty(false);
        else schedule(AUTOSAVE_MS);
      } else if (!FINAL.has(r.error?.code)) {
        schedule(RETRY_MS);
      } else {
        console.error("ddc editor: the ERP refused the draft", r.error);
      }
    } finally {
      st.saving = false;
    }
  }

  async function readFile(path) {
    const r = await ask({ type: "read_file", path });
    if (!r.ok) throw new Error(`${path}: ${r.error?.message || r.error?.code || "not read"}`);
    return bytesOf(r.bytes);
  }

  function applyMode() {
    const view = !editable();
    if (view !== ed.view_only()) ed.set_view_only(view); // switching clears the selection
  }

  async function onOpen(m) {
    st.open = m;
    const gen = ++st.loading;
    clearTimeout(st.timer);
    ed.set_locale(m.locale || "en");
    ed.set_theme(m.theme === "dark" ? "dark" : "light");
    ed.set_view_only(true);
    try {
      const tip = m.revision;
      const source = tip?.files?.find((f) => f.role === "source") || tip?.files?.find((f) => f.path.endsWith(".ddc"));
      const tipModel = source ? await readFile(source.path) : null;
      if (gen !== st.loading || st.gone) return;
      // the draft diffs against the chain tip, and the selection carries its classes and types (view
      // mode too): rows from its elements.parquet (zstd or snappy), else from its model (no parquet, or
      // an unreadable one)
      ed.clear_parent();
      const rows = tip?.files?.find((f) => f.role === "elements") || tip?.files?.find((f) => f.path === "elements.parquet");
      let rowsRead = false;
      if (tip && rows) {
        try {
          const parquet = await readFile(rows.path);
          if (gen !== st.loading || st.gone) return;
          ed.set_parent_rows(tip.revision_id, parquet);
          rowsRead = true;
        } catch (e) {
          console.warn(`ddc editor: ${rows.path} not read, the tip model gives the rows`, e);
        }
      }
      if (tip && tipModel && !rowsRead) {
        try {
          ed.set_parent_model(tip.revision_id, tipModel);
        } catch (e) {
          console.error("ddc editor: the tip rows were not read", e);
        }
      }
      if (m.draft) {
        const pkg = await readFile(DRAFT_FILE);
        if (gen !== st.loading || st.gone) return;
        ed.load_package(pkg);
        st.draftId = m.draft.revision_id;
      } else {
        if (tipModel) ed.load_model(tipModel);
        st.draftId = newId();
      }
    } catch (e) {
      console.error("ddc editor: the model was not loaded", e);
    } finally {
      if (gen === st.loading && !st.gone) {
        st.quiet = ed.revision();
        st.loading = 0;
        st.dirty = false;
        applyMode();
      }
    }
  }

  ed.on_change((rev) => {
    if (st.loading || rev === st.quiet || !editable()) return;
    setDirty(true);
    schedule(AUTOSAVE_MS);
  });
  ed.on_select((json) => post({ type: "select", elements: JSON.parse(json).slice(0, 5000) }));
  // the AI panel's provider: the ERP runs brief_from_text and asks the person; no AI service is called here
  ed.set_ai_host(async (reqText) => {
    const req = JSON.parse(reqText);
    if (req.op === "models") return JSON.stringify({ bridge: AI_WIRE, models: ["erp"] });
    const done = draftResult(req);
    if (done) {
      const text = done.is_error ? `The brief was not drafted: ${String(done.content).slice(0, 500)}` : "Drafted from the ERP brief: check the preview, then press Apply.";
      return JSON.stringify({ bridge: AI_WIRE, text, tool_calls: [], finish: "stop" });
    }
    if (!st.open?.ai?.available) return JSON.stringify(aiReply({ status: "unavailable" }));
    const text = lastUserText(req);
    if (!text) return JSON.stringify({ bridge: AI_WIRE, error: { kind: "blocked", message: "Describe the building in words: the ERP turns the text into brief values." } });
    const r = await ask({ type: "ai_request", action: "brief_from_text", text });
    let call = null;
    if (r.status === "confirmed" && r.brief && !st.gone) {
      try {
        call = JSON.parse(ed.brief_tool_call(JSON.stringify(r.brief)));
      } catch (e) {
        call = { error: String(e).slice(0, 300) };
      }
    }
    return JSON.stringify(aiReply(r, call, new Set((req.tools || []).map((t) => t.name))));
  });
  const onResize = () => st.open && !st.loading && applyMode();
  window.addEventListener("resize", onResize);

  function refuse(supported) {
    st.refused = true;
    ed.set_view_only(true);
    const note = document.createElement("div");
    note.setAttribute("role", "alert");
    note.style.cssText = "position:absolute;inset:auto 16px 16px 16px;padding:12px;background:#fff;color:#222;border:1px solid #c33;font:14px sans-serif";
    note.textContent = `This browser editor speaks bridge ${BRIDGE}, the ERP ${(supported || []).join(", ") || "none"}: open the project in the desktop editor.`;
    element.appendChild(note);
    st.note = note;
  }

  return {
    post(msg) {
      if (!msg || msg.bridge !== BRIDGE || st.gone) return;
      if (st.refused) return;
      switch (msg.type) {
        case "open":
          onOpen(msg);
          break;
        case "refused":
          refuse(msg.supported);
          break;
        case "settings":
          ed.set_locale(msg.locale || "en");
          ed.set_theme(msg.theme === "dark" ? "dark" : "light");
          break;
        case "select":
          ed.select_keys(JSON.stringify((msg.elements || []).map((e) => e.element_key)));
          break;
        case "file":
        case "save_result":
        case "ai_result": {
          const resolve = st.waiting.get(msg.request_id);
          st.waiting.delete(msg.request_id);
          resolve?.(msg);
          break;
        }
        default:
          break;
      }
    },
    unmount() {
      st.gone = true;
      clearTimeout(st.timer);
      window.removeEventListener("resize", onResize);
      for (const resolve of st.waiting.values()) resolve({ ok: false, status: "failed", error: { code: "other", message: "unmounted" } });
      st.waiting.clear();
      ed.destroy();
      ed.free();
      canvas.remove();
      st.note?.remove();
    },
  };
}
