"""One hyperfine run: render a benchmark template N times with one engine.

    python benches/hf_render.py <engine> <bench> <n>

engine: py        odoo ir_qweb.render() (compiles at every call)
        py-cached ir.qweb with its compiled-code cache (what Odoo does)
        rs        qweb_rs.render() (same API as ir_qweb.render)
        rs-engine qweb_rs.Engine, compiled templates cached
        none      startup only (imports + data), to subtract
Every engine imports the same modules and renders once before timing starts,
so the difference between two commands is the rendering time.
"""

import sys

import qweb_rs  # noqa: E402
from compare import BENCHES, make_loader, reference_renderer  # noqa: E402
from odoo.addons.base.models.ir_qweb import render as py_render  # noqa: E402

engine, bench, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
templates, values = BENCHES[bench]
load = make_loader(templates)
if engine == "py":

    def fn():
        return py_render("main", values, load)
elif engine == "py-cached":
    ref = reference_renderer()

    def fn():
        return ref("main", values, load)
elif engine == "rs":

    def fn():
        return qweb_rs.render("main", values, load)
elif engine == "rs-engine":
    eng = qweb_rs.Engine(trust_names=True)
    vals = {**values, "true": True, "false": False}

    def fn():
        return eng.render("main", vals, load)
elif engine == "none":

    def fn():
        return None

    n = 0
else:
    raise SystemExit(f"unknown engine {engine}")
fn()
for _ in range(n):
    fn()
