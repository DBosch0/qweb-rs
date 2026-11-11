"""Differential fuzzing of the browser dialect (qweb-wasm ``TemplateStore``)
against the original ``web/static/src/core/template_inheritance.js``, in
headless Chrome.

    python tests/inherit_js/fuzz_browser.py [iterations] [seed]

Builds a self-contained HTML page (original module + wasm-bindgen glue +
base64 wasm + fuzzer), runs it with ``--dump-dom`` and reports mismatches.
Requires ``target/wasm-pkg/no-modules`` (see README: building the WASM package).
"""
import base64
import html
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ODOO = os.environ.get('ODOO_PATH')  # the Odoo source tree (required)
CHROME = os.environ.get('CHROME', 'google-chrome-stable')
PKG = os.path.join(ROOT, 'target', 'wasm-pkg', 'no-modules')

FUZZER = r"""
var odoo = { debug: "" };
function rng(seed) {
    let s = seed >>> 0 || 1;
    return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; };
}
const TAGS = ["div", "p", "span", "t", "section", "field"];
const CLASSES = ["a", "b", "c", "o_x", "abc"];
const WS = ["", "", "\n", "\n    ", "\n        ", " ", "\n  \n  ", "x", " y ", "\t", "\n\n", " Hop! "];
function gen(r) {
    const pick = (a) => a[Math.floor(r() * a.length)];
    let ids = 0;
    function attrs(tag) {
        const a = [];
        ids++;
        if (r() < 0.7) a.push(["id", String(ids)]);
        if (r() < 0.4) a.push(["class", [pick(CLASSES), pick(CLASSES)].join(pick([" ", "  ", ""]))]);
        if (tag === "field" || r() < 0.2) a.push(["name", pick(["x", "y", "z"])]);
        return a;
    }
    const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");
    function element(depth) {
        const tag = pick(TAGS);
        let s = "<" + tag + attrs(tag).map(([k, v]) => ` ${k}="${esc(v)}"`).join("") + ">";
        if (r() < 0.6) s += pick(WS);
        if (depth > 0) {
            const n = Math.floor(r() * 4);
            for (let i = 0; i < n; i++) {
                const k = r();
                s += k < 0.1 ? "<!--c-->" : element(depth - 1);
                if (r() < 0.7) s += pick(WS);
            }
        }
        return s + "</" + tag + ">";
    }
    const arch = '<t t-name="web.A">' + pick(WS) + element(2) + pick(WS) + element(2) + pick(WS) + "</t>";
    const doc = new DOMParser().parseFromString(arch, "text/xml");
    const nodes = [...doc.documentElement.querySelectorAll("*")];
    function locator(allowRoot) {
        const k = r();
        const target = nodes.length ? pick(nodes) : null;
        if (!target || k < 0.08) return `<xpath expr="${pick([".", "/t", "//nothing", "./*[1]", "//*[text()='x']"])}"`;
        const id = target.getAttribute("id");
        const cls = target.getAttribute("class");
        if (k < 0.7) {
            let expr;
            if (id && r() < 0.5) expr = pick([`//*[@id='${id}']`, `//${target.tagName}[@id=${id}]`]);
            else if (cls && r() < 0.5) expr = `//${target.tagName}[hasclass('${cls.trim().split(/\s+/)[0]}')]`;
            else expr = pick([`//${target.tagName}`, `(//${target.tagName})[last()]`, `./${target.tagName}`,
                `//${target.tagName}/..`, `//${target.tagName}/following-sibling::*[1]`, `//${target.tagName}[contains(@class, 'a')]`]);
            return `<xpath expr="${expr}"`;
        }
        return "<" + target.tagName + (id && r() < 0.8 ? ` id="${id}"` : "");
    }
    function content(depth) {
        let s = r() < 0.6 ? pick(WS) : "";
        const n = Math.floor(r() * 4);
        for (let i = 0; i < n; i++) {
            const k = r();
            if (k < 0.15) s += `<xpath expr="//*[@id='${1 + Math.floor(r() * ids)}']" position="move"/>`;
            else if (k < 0.22) s += "<!--spec-->";
            else if (k < 0.3) s += "<b>$0</b>";
            else s += element(depth);
            if (r() < 0.7) s += pick(WS);
        }
        return s;
    }
    function op() {
        let loc = locator();
        const tag = loc.startsWith("<xpath") ? "xpath" : loc.slice(1).split(" ")[0];
        const pos = pick(["inside", "after", "before", "replace", "replace", "attributes", null, "bad"]);
        if (pos) loc += ` position="${pos}"`;
        if (pos === "replace" && r() < 0.4) loc += ` mode="${pick(["inner", "outer", "bad"])}"`;
        let body = "";
        if (pos === "attributes") {
            const n = 1 + Math.floor(r() * 3);
            for (let i = 0; i < n; i++) {
                const name = pick(["class", "style", "id", "t-if"]);
                const k = r();
                if (k < 0.35) body += `<attribute name="${name}">${pick(["", "v", "a b"])}</attribute>`;
                else body += `<attribute name="${name}"${r() < 0.6 ? ` separator="${pick([" ", ",", ";"])}"` : ""} ${pick(["add", "remove"])}="${pick(["a", "b c", "a, d", "o_x"])}"/>`;
                body += pick(WS);
            }
        } else {
            body = content(1);
        }
        return loc + ">" + body + "</" + tag + ">";
    }
    let ops = "<t" + (r() < 0.5 ? ' t-inherit="web.A"' : "") + ">" + pick(WS);
    const n = 1 + Math.floor(r() * 3);
    for (let i = 0; i < n; i++) ops += op() + pick(WS);
    ops += "</t>";
    return { arch, ops, debug: r() < 0.3 ? "1" : "", url: pick(["test/from_op", "", "/web/static/src/x.xml"]) };
}
function build(doc, j) {
    switch (j[0]) {
        case 3: return doc.createTextNode(j[1]);
        case 8: return doc.createComment(j[1]);
        case 7: return doc.createProcessingInstruction(j[1], j[2]);
    }
    const el = doc.createElementNS(j[2], j[1]);
    for (const [q, ns, v] of j[3]) ns ? el.setAttributeNS(ns, q, v) : el.setAttribute(q, v);
    for (const c of j[4]) el.appendChild(build(doc, c));
    return el;
}
const ser = new XMLSerializer();
function runOriginal(c) {
    odoo.debug = c.debug;
    const warns = [];
    const ow = console.warn;
    console.warn = (m) => warns.push(m);
    try {
        const root = new DOMParser().parseFromString(c.arch, "text/xml").documentElement;
        root.setAttribute("t-translation-context", "from_target");
        const ops = new DOMParser().parseFromString(c.ops, "text/xml").documentElement;
        const res = applyInheritance(root, ops, c.url);
        applyContextToTextNode();
        return { out: ser.serializeToString(res), warns };
    } catch (e) {
        applyContextToTextNode();
        return { out: "ERROR " + e.message, warns, error: true };
    } finally {
        console.warn = ow;
    }
}
function runWasm(store, c) {
    store.setDebug(!!c.debug);
    store.checkpoint();
    try {
        const root = store.parse(c.arch);
        store.setAttribute(root, "t-translation-context", "from_target");
        const ops = store.parse(c.ops);
        const res = store.applyInheritance(root, ops, c.url);
        store.applyTextContexts(res);
        const doc = new Document();
        doc.appendChild(build(doc, JSON.parse(store.toJson(res))));
        return { out: ser.serializeToString(doc.documentElement), warns: store.takeWarnings() };
    } catch (e) {
        return { out: "ERROR " + e.message, warns: store.takeWarnings(), error: true };
    } finally {
        store.release([]);
    }
}
function main() {
    const out = document.getElementById("out");
    try {
        wasm_bindgen.initSync({ module: Uint8Array.from(atob(WASM), (c) => c.charCodeAt(0)) });
        const store = new wasm_bindgen.TemplateStore(false);
        let mismatches = 0, errors = 0, shown = [];
        for (let i = 0; i < ITERATIONS; i++) {
            const c = gen(rng(SEED * 1000003 + i + 1));
            const a = runOriginal(c);
            const b = runWasm(store, c);
            errors += a.error ? 1 : 0;
            const same = (a.error && b.error) || (a.out === b.out && JSON.stringify(a.warns) === JSON.stringify(b.warns));
            if (!same) {
                mismatches++;
                if (shown.length < 5) shown.push({ i, c, original: a, wasm: b });
            }
        }
        out.textContent = JSON.stringify({ iterations: ITERATIONS, errors, mismatches, shown, nodes: store.nodeCount() });
    } catch (e) {
        out.textContent = JSON.stringify({ crash: String(e && e.stack || e) });
    }
}
"""


def main():
    iterations = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    if not ODOO:
        sys.exit('set ODOO_PATH to the Odoo source tree')
    original = open(os.path.join(ODOO, 'addons/web/static/src/core/template_inheritance.js'), encoding='utf-8').read()
    original = re.sub(r'^export ', '', original, flags=re.M)
    glue = open(os.path.join(PKG, 'qweb_wasm.js'), encoding='utf-8').read()
    wasm = base64.b64encode(open(os.path.join(PKG, 'qweb_wasm_bg.wasm'), 'rb').read()).decode()
    page = (
        '<!doctype html><html><body><pre id="out">not run</pre>'
        f'<script>{glue}</script>'
        f'<script>const WASM = "{wasm}"; const ITERATIONS = {iterations}; const SEED = {seed};</script>'
        f'<script>{FUZZER}\n{original}\nmain();</script></body></html>'
    )
    with tempfile.NamedTemporaryFile('w', encoding='utf-8', suffix='.html', delete=False) as f:
        f.write(page)
    try:
        dom = subprocess.run(
            [CHROME, '--headless=new', '--disable-gpu', '--no-sandbox', '--virtual-time-budget=600000',
             '--dump-dom', 'file://' + f.name],
            capture_output=True, text=True, timeout=900,
        ).stdout
    finally:
        os.unlink(f.name)
    m = re.search(r'<pre id="out">(.*?)</pre>', dom, re.S)
    if not m:
        print('no result', dom[:2000])
        sys.exit(2)
    import json
    res = json.loads(html.unescape(m.group(1)))
    if 'crash' in res:
        print(res['crash'])
        sys.exit(2)
    for s in res['shown']:
        print(f"--- mismatch #{s['i']} debug={s['c']['debug']!r} url={s['c']['url']!r}")
        print('arch:    ', s['c']['arch'])
        print('ops:     ', s['c']['ops'])
        print('original:', s['original']['out'], s['original']['warns'])
        print('wasm:    ', s['wasm']['out'], s['wasm']['warns'])
    print(f"{res['iterations']} cases, {res['errors']} expected errors, {res['mismatches']} mismatches, "
          f"{res['nodes']} live nodes left")
    sys.exit(1 if res['mismatches'] else 0)


if __name__ == '__main__':
    main()
