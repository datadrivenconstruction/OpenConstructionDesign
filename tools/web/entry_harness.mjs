// Drives tools/web/entry.js with a stand-in wasm module and page, prints what it posted and called
// as JSON (binary fields as "<n bytes>"). Run by tools/test_web_entry.py: node entry_harness.mjs <entry.js>
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const FAKE = `
export default async function init(o) { globalThis.__ddc.calls.push(["init", String(o.module_or_path).split("/").pop()]); }
export class WebEditor {
  constructor() { this.rev = 1; this.view = false; globalThis.__ddc.ed = this; }
  #c(name, ...a) { globalThis.__ddc.calls.push([name, ...a]); }
  async start(id, ex, gpu) { this.#c("start", ex, gpu); }
  set_view_only(on) { this.view = on; this.#c("set_view_only", on); }
  view_only() { return this.view; }
  revision() { return this.rev; }
  load_model(b) { this.#c("load_model", b.length); }
  load_package(b) { this.#c("load_package", b.length); }
  set_parent_model(id, b) { this.#c("set_parent_model", id, b.length); }
  set_parent_rows(id, b) { this.#c("set_parent_rows", id, b.length); }
  clear_parent() { this.#c("clear_parent"); }
  brief_tool_call(j) { this.#c("brief_tool_call", JSON.parse(j).name); return JSON.stringify({ name: "generate_project", args: { spec: { typology: "apartment_section" }, why: "w" } }); }
  save_package(pid, name, draft, depth) { this.#c("save_package", pid, draft ?? null); return new Uint8Array(100).fill(7); }
  set_locale(l) { this.#c("set_locale", l); }
  set_theme(t) { this.#c("set_theme", t); }
  select_keys(j) { this.#c("select_keys", j); return 1; }
  on_change(cb) { this.change = cb; }
  on_select(cb) { this.select = cb; }
  set_ai_host(cb) { this.ai = cb; }
  on_assets(cb) { this.assets = cb; }
  add_asset(p, b) { this.#c("add_asset", p, b.length); }
  destroy() { this.#c("destroy"); }
  free() { this.#c("free"); }
}`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const listeners = new Set();

function page(width) {
  globalThis.window = {
    innerWidth: width,
    addEventListener: (_, f) => listeners.add(f),
    removeEventListener: (_, f) => listeners.delete(f),
  };
  globalThis.matchMedia = () => ({ matches: true }); // a fine pointer
  globalThis.document = {
    createElement: (tag) => ({ tag, style: {}, attrs: {}, setAttribute(k, v) { this.attrs[k] = v; }, remove() { this.removed = true; } }),
  };
}

function element() {
  return { children: [], appendChild(c) { this.children.push(c); } };
}

function plain(m) {
  const out = {};
  for (const [k, v] of Object.entries(m)) out[k] = v instanceof Uint8Array || v instanceof ArrayBuffer ? `<${v.byteLength} bytes>` : v;
  return out;
}

async function run(mount, name, width, script) {
  page(width);
  globalThis.__ddc = { calls: [] };
  const posted = [];
  const el = element();
  const host = { post: (m) => posted.push(m) };
  const h = await mount(el, host);
  const ed = globalThis.__ddc.ed;
  const extra = await script({ h, ed, posted, el, last: (t) => posted.filter((m) => m.type === t).at(-1) });
  return { name, posted: posted.map(plain), raw: posted, calls: globalThis.__ddc.calls, el: el.children.map((c) => ({ tag: c.tag, removed: !!c.removed, role: c.attrs.role || null })), listeners: listeners.size, ...extra };
}

const TIP = "9c1d2e3f-4a5b-4c6d-8e7f-0a1b2c3d4e5f";
const DRAFT = "5b0c7e21-9d3f-4a8e-b6c1-2f4d8a9e0c13";
const open = (mode, draft, rows = false) => ({
  bridge: 1, type: "open", project: { id: "746402cb-14a8-430c-b3b9-9c6908fa75d7", name: "Wohnhaus" },
  brief: null, brief_version: null,
  revision: { revision_id: TIP, number: 2, status: "published", files: [
    { path: "model.ddc", role: "source", url: "/api/x/model.ddc", bytes: 10 },
    { path: "geometry.glb", role: "geometry", url: "/api/x/geometry.glb", bytes: 20 },
    ...(rows ? [{ path: "elements.parquet", role: "elements", url: "/api/x/elements.parquet", bytes: 40 }] : [])] },
  draft: draft ? { revision_id: DRAFT, parent_revision_id: TIP, updated_at: "2026-11-15T10:42:00Z", element_count: 3 } : null,
  mode, locale: "de", theme: "light", permissions: { can_edit: mode === "edit", can_accept: false }, ai: { available: true },
});

async function answerFiles(h, posted, from) {
  const done = new Set();
  for (let i = 0; i < 8; i++) {
    await sleep(5);
    for (const m of posted.slice(from).filter((m) => m.type === "read_file" && !done.has(m))) {
      done.add(m);
      h.post({ bridge: 1, type: "file", request_id: m.request_id, ok: true, bytes: new Uint8Array({ "model.ddc": 10, "elements.parquet": 40 }[m.path] ?? 30) });
    }
  }
}

const entry = process.argv[2];
const dir = mkdtempSync(join(tmpdir(), "ddc-entry-"));
copyFileSync(entry, join(dir, "ddc-editor.js"));
writeFileSync(join(dir, "ddc_web_editor.js"), FAKE);
// the bundle folder: a manifest with two on-demand files, one of them altered after hashing
const hex = async (b) => [...new Uint8Array(await crypto.subtle.digest("SHA-256", b))].map((x) => x.toString(16).padStart(2, "0")).join("");
mkdirSync(join(dir, "i18n"));
mkdirSync(join(dir, "fonts"));
const de = new TextEncoder().encode('{"a":"b"}');
writeFileSync(join(dir, "i18n", "de.json"), de);
writeFileSync(join(dir, "fonts", "NotoSansSC.ttf"), "altered");
writeFileSync(join(dir, "i18n", "xx.json"), "{}");
writeFileSync(join(dir, "manifest.json"), JSON.stringify({ files: [
  { path: "i18n/de.json", bytes: de.length, sha256: await hex(de), load: "on_demand" },
  { path: "fonts/NotoSansSC.ttf", bytes: 7, sha256: "0".repeat(64), load: "on_demand" },
] }));
globalThis.fetch = async (url) => {
  try {
    const b = readFileSync(fileURLToPath(url));
    return { ok: true, status: 200, json: async () => JSON.parse(b), arrayBuffer: async () => b.buffer.slice(b.byteOffset, b.byteOffset + b.length) };
  } catch {
    return { ok: false, status: 404 };
  }
};
const { mount } = await import(pathToFileURL(join(dir, "ddc-editor.js")).href);
const out = [];

out.push(await run(mount, "edit", 1400, async ({ h, ed, posted, last }) => {
  h.post(open("edit", true, true));
  await answerFiles(h, posted, 0);
  const loaded = globalThis.__ddc.calls.length;
  ed.rev = 2; ed.change(2);
  const dirtyAt = posted.length;
  // the autosave timer is 1.5 s; a loaded machine fires it late, so wait for the message
  for (let t = 0; t < 100 && !last("save"); t++) await sleep(50);
  const save = last("save");
  const digest = save ? [...new Uint8Array(await crypto.subtle.digest("SHA-256", new Uint8Array(100).fill(7)))].map((x) => x.toString(16).padStart(2, "0")).join("") : null;
  h.post({ bridge: 1, type: "save_result", request_id: save?.request_id, ok: true, draft: { revision_id: DRAFT, updated_at: "2026-11-16T10:00:00Z", element_count: 3 } });
  await sleep(5);
  ed.select('[{"element_key":"b1/s01/wall","ifc_class":"IfcWall","type_key":"wall/brick"},{"element_key":"x/slab/0f"}]');
  h.post({ bridge: 1, type: "select", elements: [{ element_key: "b1/s02/wall" }] });
  h.post({ bridge: 1, type: "settings", locale: "ru", theme: "dark" });
  const user = [{ role: "user", parts: [{ type: "text", text: "Fuenf Geschosse, Mauerwerk" }] }];
  const tools = [{ name: "generate_project" }, { name: "revise_project" }];
  const pending = ed.ai(JSON.stringify({ bridge: "ddc-web-bridge/1", op: "chat", messages: user, tools }));
  await sleep(5);
  const ask = last("ai_request");
  h.post({ bridge: 1, type: "ai_result", request_id: ask?.request_id, status: "confirmed", brief_version: 4, brief: { schema: "ddc-brief/1" }, confirmed: ["/massing/floors_above"] });
  const reply = JSON.parse(await pending);
  const call = reply.tool_calls?.[0];
  const after = JSON.parse(await ed.ai(JSON.stringify({ bridge: "ddc-web-bridge/1", op: "chat", tools, messages: [...user,
    { role: "assistant", parts: [{ type: "tool_call", id: call?.id, name: call?.name, args: call?.args }] },
    { role: "tool", parts: [{ type: "tool_result", id: call?.id, content: "{\"item\": 1}", is_error: false }] }] })));
  const asks = posted.filter((m) => m.type === "ai_request").length;
  window.innerWidth = 800;
  for (const f of listeners) f();
  ed.rev = 3; ed.change(3);
  await sleep(5);
  const afterNarrow = posted.length;
  h.unmount();
  return { loaded, dirtyAt, digest, reply, after, asks, afterNarrow };
}));

out.push(await run(mount, "edit_no_rows", 1400, async ({ h, ed, posted }) => {
  h.post(open("edit", false));
  await answerFiles(h, posted, 0);
  ed.rev = 2; ed.change(2);
  await sleep(1700);
  h.unmount();
  return {};
}));

out.push(await run(mount, "view", 1400, async ({ h, ed, posted }) => {
  h.post(open("view", false, true));
  await answerFiles(h, posted, 0);
  ed.rev = 2; ed.change(2);
  await sleep(1700);
  h.unmount();
  return {};
}));

out.push(await run(mount, "narrow", 700, async ({ h, ed, posted }) => {
  h.post(open("edit", false));
  await answerFiles(h, posted, 0);
  ed.rev = 2; ed.change(2);
  await sleep(50);
  h.unmount();
  return {};
}));

out.push(await run(mount, "assets", 1400, async ({ h, ed }) => {
  ed.assets(JSON.stringify(["i18n/de.json", "fonts/NotoSansSC.ttf", "i18n/xx.json", "i18n/none.json"]));
  for (let t = 0; t < 40 && !globalThis.__ddc.calls.some((c) => c[0] === "add_asset"); t++) await sleep(10);
  await sleep(20);
  h.unmount();
  return {};
}));

out.push(await run(mount, "refused", 1400, async ({ h, posted }) => {
  h.post({ bridge: 1, type: "refused", supported: [2] });
  h.post(open("edit", false));
  await sleep(20);
  h.unmount();
  return {};
}));

for (const r of out) delete r.raw;
rmSync(dir, { recursive: true, force: true });
console.log(JSON.stringify(out));
