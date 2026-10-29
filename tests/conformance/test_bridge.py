"""Custom directives through a ``qweb_rs.Bridge`` (compile hooks and proxies)."""

import pytest
import qweb_rs


ORDER = [
    "elif", "else", "debug", "log", "groups", "as", "foreach", "if", "call-assets",
    "lang", "options", "call", "att", "field", "out", "tag-open", "set",
    "inner-content", "tag-close",
]  # fmt: skip


class DirectiveBridge(qweb_rs.Bridge):
    flags = qweb_rs.FLAG_COMPILE_HOOKS

    def __init__(self, templates, handlers):
        self.templates = templates
        self.handlers = handlers

    def load(self, ref):
        return self.templates[ref]

    def compile_hooks(self):
        # the whole order (Odoo's `_directives_eval_order`), custom ones first
        return {"order": [*self.handlers, *ORDER]}

    def has_directive(self, name):
        return name in self.handlers

    def compile_directive(self, name, el, compiler):
        self.handlers[name](el, compiler)


def render(templates, handlers, values=None):
    bridge = DirectiveBridge(templates, handlers)
    return qweb_rs.Engine().render("main", values or {}, bridge=bridge)


def shout(el, compiler):
    # edit the element and let the next directives compile it
    compiler.append_text(f"[{el.attrib.pop('t-shout').upper()}]")


def box(el, compiler):
    # wrap the element and compile the wrapper (like `t-snippet-call`)
    el.attrib.pop("t-box")
    compiler.compile_node(compiler.wrap(el, "div", {"class": "box"}))


def test_nested_directives():
    # The outer handler compiles a node whose child has a directive: the
    # inner handler gets its own proxies while the outer compiler is busy.
    out = render(
        {"main": '<t t-name="main"><t t-box=""><b t-shout="hi" t-out="x"/></t></t>'},
        {"shout": shout, "box": box},
        {"x": "<x>"},
    )
    assert out == '<div class="box">[HI]<b>&lt;x&gt;</b></div>'


def test_stale_proxy():
    kept = []

    def keep(el, compiler):
        el.attrib.pop("t-keep")
        kept.append((el, compiler))

    assert render({"main": '<t t-name="main"><i t-keep=""/></t>'}, {"keep": keep}) == "<i></i>"
    el, compiler = kept[0]
    with pytest.raises(RuntimeError, match="only available"):
        el.tag
    with pytest.raises(RuntimeError, match="only available"):
        compiler.append_text("x")


def test_reentrant_proxy():
    outer = []

    def keep(el, compiler):
        el.attrib.pop("t-keep")
        outer.append(el)
        compiler.compile_node(compiler.wrap(el, "t"))

    def reenter(el, compiler):
        el.attrib.pop("t-reenter")
        # the outer compiler is in compile_node: its proxies must not be used
        with pytest.raises(RuntimeError, match="busy"):
            outer[0].tag

    out = render(
        {"main": '<t t-name="main"><t t-keep=""><b t-reenter=""/></t></t>'},
        {"keep": keep, "reenter": reenter},
    )
    assert out == "<b></b>"
    # the directive is over: its proxies are expired
    with pytest.raises(RuntimeError, match="only available"):
        outer[0].tag
