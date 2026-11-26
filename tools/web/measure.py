# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Browser build check: serves tools/web, opens it in headless Chromium (WebGL2 via SwiftShader),
waits for the first painted frame of the DE example, edits it through the page API and prints one
JSON line (sizes, first paint, wasm memory, edit results). Needs `pip install playwright` + chromium.

usage: python tools/web/measure.py [--shot out.png] [--headed] [--pkg DIR] [--model FILE.ddc] [--check] [--ledger F]
  --pkg DIR  wasm-bindgen output to load instead of tools/web/pkg (e.g. dist/web-editor)
  --model F  start empty and load this model instead of generating the DE example
  --check    exit 1 on a start error, a panic or console error, or a failed edit/round trip
  --rows F   elements.parquet the page reads as a draft's chain tip (e.g. a desktop-published zstd one)
  --ledger F append the result line (with the commit) to F, e.g. tools/web/measure_ledger.jsonl
A bundle with on-demand files (DIR/i18n) also switches to Chinese and checks that its catalog and font arrive.
"""
import functools
import gzip
import http.server
import json
import pathlib
import subprocess
import sys
import threading
import time

from playwright.sync_api import sync_playwright

ROOT = pathlib.Path(__file__).resolve().parent
PKG = pathlib.Path(sys.argv[sys.argv.index("--pkg") + 1]).resolve() if "--pkg" in sys.argv else ROOT / "pkg"
WASM = PKG / "ddc_web_editor_bg.wasm"
ZH_ASSETS = ["fonts/NotoSansSC.ttf", "i18n/zh.json"]
MODEL = pathlib.Path(sys.argv[sys.argv.index("--model") + 1]).resolve() if "--model" in sys.argv else None
ROWS = pathlib.Path(sys.argv[sys.argv.index("--rows") + 1]).resolve() if "--rows" in sys.argv else None


class Quiet(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm", ".js": "text/javascript"}

    def translate_path(self, path):
        if path.startswith("/pkg/"):
            return str(PKG / path[len("/pkg/"):].split("?")[0])
        if MODEL and path.split("?")[0] == "/model.ddc":
            return str(MODEL)
        if ROWS and path.split("?")[0] == "/rows.parquet":
            return str(ROWS)
        return super().translate_path(path)

    def handle(self):
        try:
            super().handle()
        except ConnectionError:  # the page closed while the wasm was still streaming
            pass

    def log_message(self, *a):
        pass


def failures(out):
    """What makes --check fail: the page did not start, panicked, or an edit/round trip did not hold."""
    bad = [f"start: {out['error']}"] if "error" in out else []
    bad += [f"console: {c}" for c in out.get("console", [])]
    if "error" not in out:
        bad += [k for k in ("rev_changed", "reload_counts_equal") if out.get(k) is not True]
        if not out.get("moved") or out["moved"] == [0, 0]:
            bad.append("moved")
        if out.get("walls_after_delete") != (out.get("walls_before_delete") or 0) - 1:
            bad.append("walls_after_delete")
        if "rows_read" in out and out["rows_read"] is not True:
            bad.append(f"rows_read: {out['rows_read']}")
        if "assets_zh" in out and out["assets_zh"] != ZH_ASSETS:
            bad.append(f"assets_zh: {out['assets_zh']}")
    return bad


def serve():
    h = functools.partial(Quiet, directory=str(ROOT))
    s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), h)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s


def main():
    shot = sys.argv[sys.argv.index("--shot") + 1] if "--shot" in sys.argv else None
    raw = WASM.read_bytes()
    out = {"wasm_bytes": len(raw), "wasm_gz_bytes": len(gzip.compress(raw, 9))}
    srv = serve()
    url = f"http://127.0.0.1:{srv.server_address[1]}/index.html" + ("?empty" if MODEL else "")
    with sync_playwright() as p:
        b = p.chromium.launch(
            headless="--headed" not in sys.argv,
            args=["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
        )
        page = b.new_page(viewport={"width": 1400, "height": 900})
        logs = []
        page.on("console", lambda m: logs.append(f"{m.type}: {m.text}"[:400]))
        page.on("pageerror", lambda e: logs.append(f"pageerror: {e}"[:400]))
        t0 = time.time()
        page.goto(url)
        try:
            # window.ddc is the canvas (id="ddc") until the page sets it: wait for .ed
            page.wait_for_function("window.ddc && window.ddc.ed && (window.ddc.error || window.ddc.ed.first_frame_ms() > 0)", timeout=180_000)
            out["wall_s_to_first_frame"] = round(time.time() - t0, 2)
            err = page.evaluate("window.ddc.error || null")
            if MODEL and not err:
                t1 = time.time()
                err = page.evaluate("""async () => {
                    try { window.ddc.ed.load_model(new Uint8Array(await (await fetch('/model.ddc')).arrayBuffer())); return null }
                    catch (e) { return 'load_model: ' + e } }""")
                out["model_bytes"], out["load_model_s"] = MODEL.stat().st_size, round(time.time() - t1, 2)
        except Exception as e:  # a panic aborts the module: no frame, no error field
            err = f"no first frame: {str(e).splitlines()[0]}"
        if err:
            out["error"] = err
        else:
            page.wait_for_timeout(1500)
            out.update(page.evaluate(
                """() => {
                const ed = window.ddc.ed, r = {};
                r.init_ms = Math.round(window.ddc.t_init);
                r.first_frame_ms = Math.round(ed.first_frame_ms());
                r.wasm_memory_mb = +(window.ddc.wasm.memory.buffer.byteLength / 1048576).toFixed(1);
                if (performance.memory) r.js_heap_mb = +(performance.memory.usedJSHeapSize / 1048576).toFixed(1);
                r.counts = JSON.parse(ed.counts());
                const lv = JSON.parse(ed.levels());
                r.levels = lv.levels.length;
                const w = ed.select_wall(0);
                const before = JSON.parse(ed.wall_line(w));
                const rev0 = ed.revision();
                r.move_msg = ed.move_selection(1.0, 0.5);
                const after = JSON.parse(ed.wall_line(w));
                r.moved = before && after ? [+(after[0]-before[0]).toFixed(3), +(after[1]-before[1]).toFixed(3)] : null;
                r.rev_changed = ed.revision() !== rev0;
                const walls0 = r.counts.wall ?? null;
                ed.select_wall(1);
                ed.delete_selection();
                r.walls_after_delete = JSON.parse(ed.counts()).wall ?? null;
                r.walls_before_delete = walls0;
                if (lv.levels.length > 1) { ed.set_level(lv.levels[1][0]); r.level_now = JSON.parse(ed.levels()).current; r.level_wanted = lv.levels[1][0]; }
                const c0 = ed.counts();
                const bytes = ed.save_model();
                r.ddc_bytes = bytes.length;
                ed.load_model(bytes);
                r.reload_counts_equal = ed.counts() === c0;
                return r;
            }"""))
            if ROWS:
                out["rows_read"] = page.evaluate("""async () => {
                    const ed = window.ddc.ed;
                    try { ed.set_parent_rows('measure-tip', new Uint8Array(await (await fetch('/rows.parquet')).arrayBuffer())); }
                    catch (e) { return String(e); } finally { ed.clear_parent(); }
                    return true;
                }""")
            if (PKG / "i18n").is_dir():
                out["assets_zh"] = page.evaluate("""async () => {
                    const ed = window.ddc.ed, got = [];
                    ed.on_assets(async (j) => { for (const p of JSON.parse(j)) {
                        const r = await fetch('/pkg/' + p);
                        if (r.ok) { ed.add_asset(p, new Uint8Array(await r.arrayBuffer())); got.push(p); } } });
                    ed.set_locale('zh');
                    for (let i = 0; i < 200 && got.length < 2; i++) await new Promise((r) => setTimeout(r, 50));
                    await new Promise((r) => setTimeout(r, 500));
                    return got.sort();
                }""")
                out["on_demand_bytes"] = sum(f.stat().st_size for d in ("i18n", "fonts") for f in (PKG / d).glob("*"))
            page.wait_for_timeout(1000)
            if shot:
                page.screenshot(path=shot)
            out["wasm_memory_mb_end"] = page.evaluate("+(window.ddc.wasm.memory.buffer.byteLength / 1048576).toFixed(1)")
        out["console"] = [l for l in logs if "error" in l.lower() or "panic" in l.lower()][:10]
        b.close()
    srv.shutdown()
    print(json.dumps(out))
    if "--ledger" in sys.argv:
        head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True, cwd=ROOT).stdout.strip()
        line = {"at": time.strftime("%Y-%m-%dT%H:%M:%S"), "commit": head, "pkg": PKG.name, **out}
        with open(sys.argv[sys.argv.index("--ledger") + 1], "a", encoding="utf-8") as f:
            f.write(json.dumps(line) + "\n")
    bad = failures(out) if "--check" in sys.argv else []
    if bad:
        print("FAIL: " + "; ".join(bad), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
