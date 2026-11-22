"""Browser smoke test of the viewer build: load a project, orbit, select, properties, plan, checks.

    python tools/web_smoke.py <build_dir> <project path relative to build_dir> [shots_dir]

Serves build_dir on localhost, drives Microsoft Edge (Playwright, channel msedge) and fails on any
console error or page error.
"""
import functools
import http.server
import json
import os
import sys
import threading
import time

from playwright.sync_api import sync_playwright


def serve(root):
    class H(http.server.SimpleHTTPRequestHandler):
        extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm",
                          ".js": "text/javascript"}

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(H, directory=root))
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def wait_state(page, pred, timeout=180):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            s = page.evaluate("window.ddc && window.ddc.state()")
        except Exception:
            s = None
        if s and pred(s):
            return s
        time.sleep(0.5)
    raise SystemExit(f"timeout; last state: {s}")


def main():
    root, project = sys.argv[1], sys.argv[2]
    shots = sys.argv[3] if len(sys.argv) > 3 else os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    srv = serve(root)
    url = f"http://127.0.0.1:{srv.server_port}/index.html?project={project}"
    errors = []
    with sync_playwright() as p:
        b = p.chromium.launch(channel="msedge", headless=True,
                              args=["--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"])
        page = b.new_page(viewport={"width": 1440, "height": 900})
        page.on("console", lambda m: m.type == "error" and errors.append(m.text))
        page.on("pageerror", lambda e: errors.append(str(e)))
        t0 = time.time()
        page.goto(url)
        s = wait_state(page, lambda s: s.get("items", 0) > 0 or "Could not" in s.get("status", ""))
        print("loaded", round(time.time() - t0, 1), "s:", s["status"])
        assert s["items"] > 0, s
        page.wait_for_timeout(800)
        page.screenshot(path=os.path.join(shots, "1-loaded.png"))

        # orbit: drag in the 3D view
        cx, cy = 720, 450
        page.mouse.move(cx, cy)
        page.mouse.down()
        for k in range(12):
            page.mouse.move(cx + 15 * k, cy - 4 * k)
            page.wait_for_timeout(16)
        page.mouse.up()
        page.wait_for_timeout(300)
        s2 = page.evaluate("window.ddc.state()")
        assert s2["camera"]["yaw"] != s["camera"]["yaw"], "orbit did not turn the camera"
        page.screenshot(path=os.path.join(shots, "2-orbit.png"))

        # select: click points around the centre until something is hit
        sel = None
        for dx, dy in [(0, 0), (40, 30), (-60, 20), (80, -40), (-30, -60), (120, 60), (-120, 80)]:
            page.mouse.click(cx + dx, cy + dy)
            page.wait_for_timeout(300)
            s3 = page.evaluate("window.ddc.state()")
            if s3["selected"]:
                sel = s3
                break
        assert sel and sel["properties"] > 0, f"nothing selected: {s3}"
        print("selected", sel["selected"], "with", sel["properties"], "properties")
        page.screenshot(path=os.path.join(shots, "3-selected.png"))

        # plan view of the first level
        page.evaluate("window.ddc.command('plan')")
        s4 = wait_state(page, lambda s: s["plan_prims"] > 0, 60)
        print("plan primitives", s4["plan_prims"])
        page.wait_for_timeout(500)
        page.mouse.click(cx, cy)
        page.wait_for_timeout(300)
        page.screenshot(path=os.path.join(shots, "4-plan.png"))

        # model checks
        page.evaluate("window.ddc.command('audit')")
        s5 = wait_state(page, lambda s: s["audit"] is not None, 120)
        print("checks", json.dumps(s5["audit"]), "|", s5["status"])
        page.wait_for_timeout(500)
        page.screenshot(path=os.path.join(shots, "5-checks.png"))
        page.evaluate("window.ddc.command('3d')")
        page.wait_for_timeout(500)
        page.screenshot(path=os.path.join(shots, "6-3d-checks.png"))
        b.close()
    srv.shutdown()
    if errors:
        print("console errors:", *errors, sep="\n  ")
        raise SystemExit(1)
    print("ok: no console errors")


if __name__ == "__main__":
    main()
