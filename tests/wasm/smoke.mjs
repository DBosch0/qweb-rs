// Smoke test of the wasm-bindgen package built by scripts/build_wasm.sh.
// Run: node tests/wasm/smoke.mjs [target/wasm-pkg/nodejs]
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";

const pkg = path.resolve(process.argv[2] ?? "target/wasm-pkg/nodejs");
const { Renderer, TemplateStore, applyInheritanceServer } = createRequire(import.meta.url)(
    path.join(pkg, "qweb_wasm.js"),
);

// Rendering (server semantics, JSON values).
const r = new Renderer();
r.addTemplate("main", '<div><t t-foreach="items" t-as="i"><b t-out="i"/></t><a t-att-title="t"/></div>');
assert.equal(
    r.render("main", JSON.stringify({ items: [1, "<x>"], t: '"q"' }), ""),
    '<div><b>1</b><b>&lt;x&gt;</b><a title="&#34;q&#34;"></a></div>',
);
assert.throws(() => r.render("missing", "", ""));
r.free();

// Server-dialect inheritance.
assert.equal(
    applyInheritanceServer(
        "<t><div class='a'/></t>",
        ["<data><xpath expr=\"//div\" position=\"inside\"><span/></xpath></data>"],
        false,
    ),
    '<t><div class="a"><span/></div></t>',
);

// Browser-dialect store.
const store = new TemplateStore(false);
const h = store.parse("<t><p x='1'/></t>");
assert.equal(store.tagName(h), "t");
store.free();

console.log("wasm smoke test: ok");
