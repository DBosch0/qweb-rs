// Smoke test of the wasm-bindgen nodejs build of scripts/build_wasm.sh.
// Run: node tests/wasm/smoke.mjs [target/wasm-pkg/nodejs]
import { createRequire } from "node:module";
import path from "node:path";
import { runChecks } from "./checks.mjs";

const pkg = path.resolve(process.argv[2] ?? "target/wasm-pkg/nodejs");
runChecks(createRequire(import.meta.url)(path.join(pkg, "qweb_wasm.js")));
console.log("wasm smoke test: ok");
