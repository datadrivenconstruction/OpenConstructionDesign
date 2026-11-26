# Browser version

The browser version is the same editor as the desktop application, compiled to WebAssembly. There
is one codebase: `ddc-app` builds for the desktop with its default feature `desktop`, and for the
browser (`wasm32-unknown-unknown`) with the feature `web`. The crate `crates/web-editor` wraps it
for the browser with wasm-bindgen, and `tools/web` holds the page entry, the build script and the
release tools.

Its main user is OpenConstructionERP: the Design section of the ERP downloads the signed bundle
once and opens the editor inside the ERP page, next to the estimate. You can also build and run it
on your own machine.

## What differs from the desktop editor

The model, the change journal, the checks, the generators, IFC import and export and the ERP
revision package all work as on the desktop. The `.ddc` project is the same SQLite file, held in
the page's memory.

Left out in the browser, because a web page cannot do them or should not:

- Lua scripts (they answer "desktop only"); recipes that ship as data still work
- operating system file dialogs, file associations, the autosave folder and crash recovery
- the update check and the title bar
- the local AI keys and the MCP server; AI requests go through the host page instead, so keys
  never reach the browser
- the converters for closed formats that run as separate programs
- the system clipboard; copy and paste work inside the editor

There are no threads in the browser, so long operations run in one go and the page waits for
them. Drawing uses WebGL2, or WebGPU where the browser has it. On touch devices and small screens
the editor opens view only; the database itself refuses every write in that mode.

## Build and run locally

Prerequisites:

- Rust with the wasm target: `rustup target add wasm32-unknown-unknown`
- clang and llvm-ar that can target wasm32 (SQLite and zstd are C and are compiled for the
  browser). On Ubuntu or Debian: `sudo apt-get install clang llvm`. On macOS, the Homebrew
  `llvm` package. On Windows, the LLVM installer, or set `CC_wasm32_unknown_unknown` and
  `AR_wasm32_unknown_unknown` to another compiler that targets wasm32.
- the wasm-bindgen command line tool in exactly the version that `Cargo.lock` names for the
  `wasm-bindgen` crate. The build script prints the install command when it is missing, for
  example `cargo install wasm-bindgen-cli --version 0.2.130 --locked`.
- Python 3 for the manifest tool
- optional: `wasm-opt` from binaryen, which makes the wasm smaller

Build the bundle (bash; on Windows use Git Bash):

```bash
bash tools/web/build_bundle.sh
# smaller wasm, slower build:
BUNDLE_PROFILE=web bash tools/web/build_bundle.sh
```

The bundle lands in `dist/web-editor/`: the wasm, its JavaScript glue, the entry module
`ddc-editor.js`, `manifest.json` with the size and SHA-256 of every file, and the locale catalogs
and fonts that are loaded on demand. The previous bundle is kept as `dist/web-editor.prev/`.

To try it without the ERP, serve the simple test page with the bundle beside it:

```bash
mkdir -p tools/web/pkg && cp -r dist/web-editor/* tools/web/pkg/
python3 -m http.server 8080 -d tools/web
```

Then open `http://localhost:8080/`. Options in the address: `?empty` starts without the sample
house, `?webgpu` uses WebGPU, `?view` and `?edit` force view only or editing. The test page loads
only English and Russian; other languages need the full entry module, as the ERP uses it.

To check that the code still compiles for the browser without building a bundle:

```bash
CC_wasm32_unknown_unknown=clang AR_wasm32_unknown_unknown=llvm-ar \
  cargo check --locked -p ddc-web-editor --target wasm32-unknown-unknown
```

CI runs this check on every change, and the workflow `web-bundle` builds the bundle on demand.

## How the ERP talks to the editor

The ERP imports `ddc-editor.js` and calls `mount(element, host)`. Every message in both directions
follows the bridge `ddc-web-bridge/1` (`crates/io/contract/schemas/web-bridge.schema.json`). The
page fetches nothing except its own bundle files: models arrive from the host, drafts go back to
the host as `.ddcrev` packages, and AI requests are answered by the host with the ERP's own AI
settings. The ERP session stays with the ERP.

## How the ERP installs and updates it

1. A browser release is built by the workflow `web-erp-release` (run by hand with a version such
   as `1.2.0-web.1`). It builds the bundle, packs it into
   `OpenConstructionDesign-v<version>-web.zip` with precompressed copies of each file, writes the
   feed `latest-web.json` (`ddc-web-release/1`: version, bridge, zip URL, size and SHA-256), signs
   the zip with minisign and checks the result with the ERP's own installer code.
2. The zip and its `.minisig` signature are attached to the GitHub release `v<version>`, and the
   feed is committed to `erp-link/release/latest-web.json` on `main`.
3. The ERP reads that feed (by default from
   `https://raw.githubusercontent.com/datadrivenconstruction/OpenConstructionDesign/main/erp-link/release/latest-web.json`,
   cached for ten minutes). The first time someone opens the editor it streams the zip to disk,
   checks size and SHA-256 against the feed, verifies the signature against the public key
   shipped with the ERP (key id `BF094D8BDEC11250`), and unpacks the bundle into
   `<ERP data dir>/design/web-editor/`. Without a public key the ERP downloads nothing.
4. To update, publish a newer release and feed. The ERP picks up the new feed, installs the new
   bundle the same way and removes the previous one. A bundle whose bridge version the ERP does
   not speak is not installed, and the ERP keeps working with what it has.

ERP administrators can change the feed URL, the key and the folder with the settings
`OE_DESIGN_WEB_FEED_URL`, `OE_DESIGN_WEB_RELEASE_PUBKEY` and `OE_DESIGN_WEB_EDITOR_DIR`; an empty
feed URL turns the full editor off.
