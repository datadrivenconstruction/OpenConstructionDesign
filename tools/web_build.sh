#!/bin/sh
# Browser viewer (crates/web) as a static folder: wasm build, wasm-bindgen, wasm-opt, page.
#   sh tools/web_build.sh [out_dir] [sample.ddc]
# Needs: rustup target wasm32-unknown-unknown; a C compiler for wasm32 (SQLite is C), given as
# CC_wasm32_unknown_unknown / AR_wasm32_unknown_unknown (clang + llvm-ar, or a zig cc wrapper);
# WASM_BINDGEN = wasm-bindgen CLI of the version in Cargo.lock; WASM_OPT = wasm-opt (optional).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/dist/web}
SAMPLE=${2:-}
BINDGEN=${WASM_BINDGEN:-wasm-bindgen}
OPT=${WASM_OPT:-$(command -v wasm-opt || true)}
TGT=${CARGO_TARGET_DIR:-$ROOT/target}
# desktop linker flags do not apply to wasm
unset RUSTFLAGS

want=$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/version = |"/, ""); print; exit}' "$ROOT/Cargo.lock")
"$BINDGEN" --version | grep -qx "wasm-bindgen $want" || { echo "wasm-bindgen $want is needed" >&2; exit 2; }

(cd "$ROOT" && cargo build --profile web -p ddc-web --target wasm32-unknown-unknown)
wasm="$TGT/wasm32-unknown-unknown/web/ddc_web.wasm"

mkdir -p "$OUT"
rm -f "$OUT/ddc_web.js" "$OUT/ddc_web_bg.wasm"
"$BINDGEN" --target web --no-typescript --out-dir "$OUT" "$wasm"
if [ -n "$OPT" ]; then
  "$OPT" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue --strip-debug --strip-producers \
    -o "$OUT/ddc_web_bg.opt.wasm" "$OUT/ddc_web_bg.wasm"
  mv -f "$OUT/ddc_web_bg.opt.wasm" "$OUT/ddc_web_bg.wasm"
fi
cp "$ROOT/crates/web/www/index.html" "$OUT/index.html"
if [ -n "$SAMPLE" ]; then
  mkdir -p "$OUT/samples"
  cp "$SAMPLE" "$OUT/samples/"
fi
ls -l "$OUT/ddc_web_bg.wasm" "$OUT/ddc_web.js"
