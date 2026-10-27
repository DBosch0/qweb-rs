// One hyperfine run: the captured view inheritance cases through the WASM
// build (server dialect), in Node.
//     node benches/hf_inherit_wasm.mjs <cases.json> [none]
// QWEB_WASM_PKG: path of another nodejs build (e.g. from wasm-pack).
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const wasm = require(process.env.QWEB_WASM_PKG || "../target/wasm-pkg/nodejs/qweb_wasm.js");
const cases = JSON.parse(readFileSync(process.argv[2], "utf8"));
if (process.argv[3] !== "none") {
    for (const c of cases) {
        try {
            wasm.applyInheritanceServer(c.source, [c.spec], c.branding);
        } catch {
            // expected errors
        }
    }
}
