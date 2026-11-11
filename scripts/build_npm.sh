#!/usr/bin/env bash
# Build the `qweb-wasm` npm package into target/npm/qweb-wasm:
#   web/   wasm-bindgen --target web     (browsers, bundlers: `await init()` first)
#   node/  wasm-bindgen --target nodejs  (Node: synchronous, import or require)
# package.json (from crates/qweb-wasm/npm, version from Cargo.toml) picks
# node/ for Node and web/ everywhere else.
# Needs the wasm32-unknown-unknown target, wasm-bindgen (same version as the
# crate) and node on PATH. Then: cd target/npm/qweb-wasm && npm publish
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT=target/npm/qweb-wasm
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)

cargo build -q -p qweb-wasm --target wasm32-unknown-unknown --profile wasm --locked
WASM=target/wasm32-unknown-unknown/wasm/qweb_wasm.wasm

rm -rf "$OUT"
mkdir -p "$OUT"
# Drop the function names (about 33 KB gzipped) and the producers section.
wasm-bindgen --target web --remove-name-section --remove-producers-section --out-dir "$OUT/web" "$WASM"
wasm-bindgen --target nodejs --remove-name-section --remove-producers-section --out-dir "$OUT/node" "$WASM"
# The package is "type": "module"; the nodejs glue is CommonJS.
echo '{ "type": "commonjs" }' > "$OUT/node/package.json"

cp crates/qweb-wasm/npm/README.md LICENSE COPYRIGHT "$OUT/"
node -e '
const fs = require("fs");
const [src, dst, version] = process.argv.slice(1);
const pkg = JSON.parse(fs.readFileSync(src, "utf8"));
pkg.version = version;
fs.writeFileSync(dst, JSON.stringify(pkg, null, 2) + "\n");
' crates/qweb-wasm/npm/package.json "$OUT/package.json" "$VERSION"
echo "built $OUT (qweb-wasm@$VERSION)"
