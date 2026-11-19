// Speed up the string arguments of the wasm-bindgen glue (JS -> WASM).
//     node scripts/patch_wasm_glue.mjs <glue.js>...
//
// wasm-bindgen's passStringToWasm0 copies a string into WASM memory one
// UTF-16 unit at a time in JS while it is ASCII, and only then calls
// TextEncoder.encodeInto. Engines implement encodeInto natively (SIMD in V8),
// so for long strings (template sources: kilobytes to megabytes) it is much
// faster to reserve the worst case (3 bytes per UTF-16 unit), encode once and
// shrink the allocation. Measured in Node on Odoo's views: -10 to -15% for
// applyInheritanceServer. Short strings keep the original loop.
//
// The glue comes from the exact wasm-bindgen version pinned in
// crates/qweb-wasm/Cargo.toml; if it changes shape, this script fails instead
// of silently doing nothing.
import { readFileSync, writeFileSync } from "node:fs";

// The ASCII loop of passStringToWasm0 starts with these two lines (indented
// one level deeper in the no-modules glue).
const ANCHOR = /^([ \t]+)let len = arg\.length;(\r?\n)\1let ptr = malloc\(len, 1\) >>> 0;\r?\n/gm;
// Inserted before them, at the same indentation and with the same line ends
// (wasm-bindgen writes \n on every OS, but a checkout may convert to \r\n).
const FAST_PATH = `// qweb-rs (scripts/patch_wasm_glue.mjs): long strings in one encodeInto.
    if (arg.length > 64) {
        const cap = arg.length * 3;
        let p = malloc(cap, 1) >>> 0;
        const { written } = cachedTextEncoder.encodeInto(arg, getUint8ArrayMemory0().subarray(p, p + cap));
        if (written !== cap) {
            p = realloc(p, cap, written, 1) >>> 0;
        }
        WASM_VECTOR_LEN = written;
        return p;
    }

`;

const files = process.argv.slice(2);
if (files.length === 0) {
    console.error("usage: node scripts/patch_wasm_glue.mjs <glue.js>...");
    process.exit(2);
}
for (const file of files) {
    const src = readFileSync(file, "utf8");
    if (src.includes("scripts/patch_wasm_glue.mjs")) {
        continue; // already patched
    }
    const start = src.indexOf("function passStringToWasm0(arg, malloc, realloc) {");
    const matches = start < 0 ? [] : [...src.slice(start).matchAll(ANCHOR)];
    if (matches.length !== 1 || !src.includes("const cachedTextEncoder")) {
        console.error(`${file}: unexpected wasm-bindgen glue, passStringToWasm0 not patched`);
        process.exit(1);
    }
    const [m] = matches;
    const at = start + m.index;
    const indent = m[1];
    const eol = m[2];
    const block = FAST_PATH.split(/\r?\n/).map((l) => (l ? indent + l.replace(/^ {4}/, "") : l)).join(eol);
    writeFileSync(file, src.slice(0, at) + block + src.slice(at));
}
