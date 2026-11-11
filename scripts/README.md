# Scripts

## `build_wasm.sh`

Builds the `qweb-wasm` WebAssembly package with `cargo` and `wasm-bindgen`.

```sh
scripts/build_wasm.sh                          # -> target/wasm-pkg/{no-modules,nodejs}
scripts/build_wasm.sh <odoo>/addons/web        # also writes <odoo>/addons/web/static/src/core/qweb_wasm/
```

It needs:
- the wasm target: `rustup target add wasm32-unknown-unknown`;
- `wasm-bindgen` **0.2.130** on `PATH`. It must match the version that
  `crates/qweb-wasm/Cargo.toml` pins:
  `cargo install wasm-bindgen-cli --version 0.2.130`.

What it does:
1. Builds `qweb-wasm` with the size-optimized `wasm` profile of the root
   `Cargo.toml` (`opt-level = "s"`, `panic = "abort"`).
2. Runs `wasm-bindgen` twice, for two targets:
   - `target/wasm-pkg/no-modules`: a global `wasm_bindgen` for the browser.
     The browser fuzzer (`tests/inherit_js/fuzz_browser.py`) uses it.
   - `target/wasm-pkg/nodejs`: a CommonJS module for Node. The WASM
     inheritance benchmark (`benches/hf_inherit_wasm.mjs`) uses it.
3. With a web addon path, it also writes the two files that Odoo's web client
   loads:
   - `qweb_wasm.js`: the no-modules glue as an ES module, with a
     `getQwebWasm()` function that instantiates the module synchronously on
     first use;
   - `qweb_wasm_bytes.js`: the `.wasm` file in base64.

   Both files are generated. Do not edit them by hand.

The script uses portable bash only, so it runs on Linux and macOS. On Windows,
use Git Bash or WSL, or build with wasm-pack instead:

```sh
wasm-pack build crates/qweb-wasm --profile wasm --target web --out-dir ../../target/wasm-pack/web
```

wasm-pack downloads the right `wasm-bindgen` by itself and runs `wasm-opt`
afterwards. It writes an npm package, but not the Odoo files. `--out-dir` is
relative to the crate directory.
