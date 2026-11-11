# qweb-rs

A Rust port of [Odoo](https://github.com/odoo/odoo)'s QWeb template engine and
its template inheritance, for Python.

The goal is **byte-identical output** to Odoo's Python engine (Odoo 20), only
faster. Every rendering rule, including whitespace handling, escaping, attribute
order and error classes, follows the original implementation. Differential tests
and fuzzers compare the two engines.

- **Rendering:** 18x to 150x faster than `ir_qweb.render()`, and 7x to 17x
  faster with cached compilation.
- **Template inheritance:** 2.1x faster than Odoo's lxml implementation.

> **Status:** experimental. `qweb_rs.render()` and `qweb_rs.apply_inheritance()`
> work on their own. The full `ir.qweb` integration needs a patched Odoo 20,
> which is not public yet.

## Install

```sh
pip install qweb-rs
```

Wheels are built for Linux (x86_64 and aarch64), macOS and Windows, for every
CPython ≥ 3.12 (stable ABI). Other platforms build from the source
distribution, which needs a Rust toolchain.

## Usage

`qweb_rs.render` has the same signature as Odoo's database-less
`odoo.addons.base.models.ir_qweb.render`:

```python
from lxml import etree
import qweb_rs

templates = {
    "hello": '<t t-name="hello"><ul><li t-foreach="items" t-as="item" t-out="item"/></ul></t>',
}

def load(name):
    return etree.fromstring(templates[name]), name

qweb_rs.render("hello", {"items": ["a", "<b>"]}, load)
# Markup('<ul><li>a</li><li>&lt;b&gt;</li></ul>')

qweb_rs.apply_inheritance(
    '<t t-name="x"><p>hi</p></t>',
    ['<xpath expr="//p" position="after"><span>new</span></xpath>'],
)
# '<t t-name="x"><p>hi</p><span>new</span></t>'
```

`qweb_rs.Engine` keeps compiled templates in a cache. It is the API that the
`ir.qweb` integration uses.

When Odoo is importable, errors are raised as Odoo's own `QWebError`.

## Links

- Source, Rust crates and WebAssembly package: <https://github.com/DBosch0/qweb-rs>
- Issues: <https://github.com/DBosch0/qweb-rs/issues>

## License

LGPL-3.0-only, like the Odoo code it is ported from.

Odoo is a trademark of Odoo S.A. This project is not affiliated with or
endorsed by Odoo S.A.
