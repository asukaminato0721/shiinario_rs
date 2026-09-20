#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
# The CLI must match the crate recorded in Cargo.lock.
WASM_BINDGEN_VERSION="$(python3 - <<'PY'
import tomllib
from pathlib import Path
lock = tomllib.loads(Path('Cargo.lock').read_text())
print(next(p['version'] for p in lock['package'] if p['name'] == 'wasm-bindgen'))
PY
)"
TOOL_DIR="$ROOT_DIR/target/wasm-tools/$WASM_BINDGEN_VERSION"
if command -v wasm-bindgen >/dev/null && [[ "$(wasm-bindgen --version)" == "wasm-bindgen $WASM_BINDGEN_VERSION" ]]; then
  BINDGEN=wasm-bindgen
else
  BINDGEN="$TOOL_DIR/bin/wasm-bindgen"
  if [[ ! -x "$BINDGEN" ]]; then
    cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked --root "$TOOL_DIR"
  fi
fi
cargo build --release --locked -p shiinario_runtime --lib --target wasm32-unknown-unknown
DIST="$ROOT_DIR/dist/wasm"
mkdir -p "$DIST/pkg"
"$BINDGEN" target/wasm32-unknown-unknown/release/shiinario_runtime.wasm --target web --out-dir "$DIST/pkg"
cp platform/wasm/{index.html,main.js,worker.js,vfs.mjs,style.css} "$DIST/"
python3 - "$DIST" <<'PY'
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
import sys
root = Path(sys.argv[1])
with ZipFile(root / 'shiinario-wasm.zip', 'w', ZIP_DEFLATED) as archive:
    for name in ['index.html', 'main.js', 'worker.js', 'vfs.mjs', 'style.css', 'pkg/shiinario_runtime.js', 'pkg/shiinario_runtime_bg.wasm']:
        archive.write(root / name, name)
PY
