"""Benchmark: Odoo's Python QWeb (compiled code cached) vs qweb_rs.

Usage: .venv/bin/python benches/compare.py
"""

import os
import sys
import time
import types

sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "tests", "conformance")
)
from reference import make_loader  # noqa: E402
from markupsafe import Markup  # noqa: E402

import qweb_rs  # noqa: E402


def reference_renderer():
    """Like ir_qweb.render() but keeping the compiled-template cache between calls."""
    from collections import defaultdict
    from lxml import etree
    from odoo.addons.base.models.ir_qweb import IrQweb
    from odoo.modules.registry import _REGISTRY_CACHES
    from odoo.tools.lru import LRU

    class MockRegistry:
        def __init__(self):
            self.db_name = None
            self._template_code__ = LRU(_REGISTRY_CACHES["templates"])

    registry = MockRegistry()

    class MockIrQWeb(IrQweb):
        _register = False
        pool = registry

        def _get_template_info(self, id_or_xmlid):
            return defaultdict(lambda: None, id=id_or_xmlid)

        def _preload_trees(self, refs):
            values = {}
            for ref in refs:
                tree, vid = self.env.context["load"](ref)
                values[ref] = values[vid] = {
                    "tree": tree,
                    "template": etree.tostring(tree, encoding="unicode"),
                    "xmlid": vid,
                    "ref": None,
                }
            return values

        def _prepare_environment(self, values):
            values["true"] = True
            values["false"] = False
            return self

    class MockCr:
        def __init__(self):
            self.cache = {}

    class MockTransaction:
        def __init__(self):
            self.ormcaches__ = {
                name: LRU(size) for name, size in _REGISTRY_CACHES.items()
            }
            self.registry = registry

    transaction = MockTransaction()

    class MockEnv(dict):
        def __init__(self):
            super().__init__()
            self.context = {}
            self.cr = MockCr()
            self.transaction = transaction
            self.registry = registry

        def __call__(self, cr=None, user=None, context=None, su=None):
            env = MockEnv()
            env.context.update(self.context if context is None else context)
            return env

    renderer = MockIrQWeb(MockEnv(), tuple(), tuple())

    def render(name, values, load):
        return renderer._render(name, values, load=load, minimal_qcontext=True)

    return render


ROWS = [
    types.SimpleNamespace(
        id=i,
        name=f"Product <{i}>",
        price=i * 1.5,
        qty=i % 7,
        active=i % 3 != 0,
        tags=["a", "b"] if i % 2 else [],
    )
    for i in range(500)
]

BENCHES = {
    "table_500_rows": (
        {
            "main": """<t t-name="main"><table class="table">
    <thead><tr><th>Id</th><th>Name</th><th>Price</th><th>Qty</th><th>Tags</th></tr></thead>
    <tbody>
        <tr t-foreach="rows" t-as="row" t-att-class="'odd' if row_odd else 'even'" t-att-data-id="row.id">
            <td t-out="row.id"/>
            <td><a t-attf-href="/product/{{ row.id }}" t-out="row.name"/></td>
            <td t-out="row.price"/>
            <td><span t-if="row.qty" t-out="row.qty"/><span t-else="">-</span></td>
            <td><t t-foreach="row.tags" t-as="tag"><span class="badge" t-out="tag"/></t></td>
        </tr>
    </tbody>
</table></t>"""
        },
        {"rows": ROWS},
    ),
    "nested_calls": (
        {
            "main": """<t t-name="main"><div class="page"><t t-foreach="rows[:200]" t-as="row"><t t-call="card"><t t-set="title" t-value="row.name"/><p t-out="row.price"/></t></t></div></t>""",
            "card": """<t t-name="card"><div class="card"><h3 t-out="row.name"/><div class="body"><t t-out="0"/></div><t t-call="footer"/></div></t>""",
            "footer": """<t t-name="footer"><footer t-attf-class="f {{ 'on' if row.active else 'off' }}">#<t t-out="row.id"/></footer></t>""",
        },
        {"rows": ROWS},
    ),
    "static_page": (
        {
            "main": '<t t-name="main"><html><head><title>T</title><meta charset="utf-8"/></head><body>'
            + "".join(
                f'<section class="s{i}"><h2>Section {i}</h2><p>Lorem ipsum dolor sit amet, consectetur adipiscing elit.</p><ul><li>a</li><li>b</li></ul></section>'
                for i in range(100)
            )
            + '<p t-out="title"/></body></html></t>'
        },
        {"title": "Hello"},
    ),
    "small_snippet": (
        {
            "main": '<t t-name="main"><div t-att-class="cls"><span t-if="user" t-out="user"/><t t-else="">anonymous</t></div></t>',
        },
        {"cls": "x", "user": "Bob <b>"},
    ),
}


def timeit(fn, min_time=0.5):
    fn()
    n, start = 0, time.perf_counter()
    while True:
        fn()
        n += 1
        elapsed = time.perf_counter() - start
        if elapsed > min_time and n >= 5:
            return elapsed / n


def main():
    from odoo.addons.base.models.ir_qweb import render as py_render

    print(
        f"{'benchmark':<16} {'py render()':>12} {'qweb_rs':>11} {'speedup':>8} | {'py cached':>11} {'qweb_rs*':>11} {'speedup':>8}"
    )
    for name, (templates, values) in BENCHES.items():
        # fresh caches per benchmark: templates are all named 'main'
        ref = reference_renderer()
        engine = qweb_rs.Engine(trust_names=True)
        load = make_loader(templates)
        a = ref("main", values, load)
        b = qweb_rs.render("main", values, load)
        c = Markup(
            engine.render("main", {**values, "true": True, "false": False}, load)
        )
        assert str(a) == str(b) == str(c), f"{name}: outputs differ"
        t_py = timeit(lambda: py_render("main", values, load), min_time=0.3)
        t_ref = timeit(lambda: ref("main", values, load))
        t_rs = timeit(lambda: qweb_rs.render("main", values, load))
        t_eng = timeit(
            lambda: engine.render(
                "main", {**values, "true": True, "false": False}, load
            )
        )
        print(
            f"{name:<16} {t_py * 1e3:10.3f}ms {t_rs * 1e3:9.3f}ms {t_py / t_rs:7.1f}x | "
            f"{t_ref * 1e3:9.3f}ms {t_eng * 1e3:9.3f}ms {t_ref / t_eng:7.1f}x"
        )
    print(
        "py render() = odoo ir_qweb.render() (recompiles every call); qweb_rs = qweb_rs.render() (same API)"
    )
    print(
        "py cached   = IrQweb with its compiled-code cache kept;    qweb_rs* = Engine(trust_names=True).render()"
    )


if __name__ == "__main__":
    main()
