// Assertions shared by the WASM smoke tests: `api` is a loaded qweb-wasm module.
import assert from "node:assert/strict";

export function runChecks({ Renderer, TemplateStore, applyInheritanceServer }) {
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
}
