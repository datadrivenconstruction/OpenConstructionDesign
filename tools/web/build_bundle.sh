#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
# Browser bundle ddc-web-editor/1 in one command: release wasm, wasm-bindgen (web), wasm-opt when
# present, entry module, manifest.json, check. Output: dist/web-editor/ of this checkout.
#   WASM_BINDGEN=path  a wasm-bindgen CLI of the crate's exact version (default: PATH, ~/.cargo/bin)
#   CC_wasm32_unknown_unknown / AR_wasm32_unknown_unknown  C compiler for SQLite and zstd (default: clang, llvm-ar)
#   CARGO_TARGET_DIR   build folder (default: target/ of this checkout)
#   WASM_OPT=path      wasm-opt (optional; skipped when absent)
#   BUNDLE_PROFILE=web cargo profile (default release; `web` = size-optimised, Cargo.toml)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/dist/web-editor"
STAGE="$ROOT/dist/web-editor.part"
WEB="$ROOT/tools/web"
PY="${PYTHON:-python}"

# the desktop linker flags break wasm; the C parts of SQLite and zstd need a compiler that targets wasm32
unset RUSTFLAGS
export CC_wasm32_unknown_unknown="${CC_wasm32_unknown_unknown:-clang}"
export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-llvm-ar}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

want=$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/version = |"/, ""); print; exit}' "$ROOT/Cargo.lock")
version=$(awk -F'"' '/^\[workspace.package\]/ {s=1} s && /^version/ {print $2; exit}' "$ROOT/Cargo.toml")
[ -n "$want" ] || { echo "no wasm-bindgen in Cargo.lock" >&2; exit 1; }
[ -n "$version" ] || { echo "no workspace version in Cargo.toml" >&2; exit 1; }

bindgen="${WASM_BINDGEN:-}"
if [ -z "$bindgen" ]; then
  for c in "$(command -v wasm-bindgen || true)" "$HOME/.cargo/bin/wasm-bindgen" "$HOME/.cargo/bin/wasm-bindgen.exe"; do
    if [ -n "$c" ] && [ -x "$c" ] && "$c" --version | grep -qx "wasm-bindgen $want"; then bindgen="$c"; break; fi
  done
fi
if [ -z "$bindgen" ] || ! "$bindgen" --version | grep -qx "wasm-bindgen $want"; then
  echo "wasm-bindgen CLI $want is needed (the crate's version in Cargo.lock). Install:" >&2
  echo "  cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 2
fi

profile="${BUNDLE_PROFILE:-release}"
echo "== cargo build ($profile, wasm32)"
cargo build --profile "$profile" -j ${BUNDLE_JOBS:-1} -p ddc-web-editor --target wasm32-unknown-unknown --message-format=short
wasm="$CARGO_TARGET_DIR/wasm32-unknown-unknown/$profile/ddc_web_editor.wasm"
[ -f "$wasm" ] || { echo "no $wasm" >&2; exit 1; }

rm -rf "$STAGE"
mkdir -p "$STAGE"
echo "== wasm-bindgen $want"
"$bindgen" --target web --no-typescript --out-dir "$STAGE" "$wasm"

opt="${WASM_OPT:-$(command -v wasm-opt || true)}"
if [ -n "$opt" ] && [ -x "$opt" ]; then
  echo "== wasm-opt"
  "$opt" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
    -o "$STAGE/ddc_web_editor_bg.opt.wasm" "$STAGE/ddc_web_editor_bg.wasm"
  mv -f "$STAGE/ddc_web_editor_bg.opt.wasm" "$STAGE/ddc_web_editor_bg.wasm"
else
  echo "== wasm-opt not found: the wasm stays as wasm-bindgen wrote it"
fi

sed "s/__VERSION__/$version/" "$WEB/entry.js" > "$STAGE/ddc-editor.js"
"$PY" "$WEB/bundle_manifest.py" assets "$STAGE" --ui "$ROOT/crates/ui"
"$PY" "$WEB/bundle_manifest.py" write "$STAGE" --version "$version" --entry ddc-editor.js

# the previous bundle is kept as dist/web-editor.prev; the new one takes its place only when complete
if [ -d "$OUT" ]; then
  rm -rf "$OUT.prev"
  mv "$OUT" "$OUT.prev"
fi
mv "$STAGE" "$OUT"
"$PY" "$WEB/bundle_manifest.py" check "$OUT"
echo "== bundle: $OUT (serve the folder; VITE_DESIGN_WEB_EDITOR_URL = <url>/manifest.json)"
