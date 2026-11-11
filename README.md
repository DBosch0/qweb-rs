# qweb-rs

A Rust port of [Odoo](https://github.com/odoo/odoo)'s QWeb template engine and
its template inheritance, with Python and WebAssembly bindings.

The goal is **byte-identical output** to Odoo's Python engine (Odoo 20), only
faster. Every rendering rule, including whitespace handling, escaping, attribute
order and error classes, follows the original implementation. Differential tests
and fuzzers compare the two engines.

- **Rendering:** 18x to 150x faster than `ir_qweb.render()`, and 7x to 17x
  faster with cached compilation (what Odoo does in production).
- **Template inheritance:** 2.1x faster than Odoo's lxml implementation, and
  the same results on every real view combination tested.
- **Portable:** the core is pure Rust with no SIMD or platform-specific
  code, and it builds for `wasm32`.

> **Status:** experimental and not yet published on PyPI, crates.io or npm.
> The full `ir.qweb` integration needs a patched Odoo (see
> [Using it in Odoo](#using-it-in-odoo)).

## Crates

| Crate | What it is |
|---|---|
| [`qweb-core`](crates/qweb-core) | XML parser (an lxml-like tree), parser for QWeb's Python expression subset, compiler to an IR, and an interpreter generic over a `Host` trait. No Python or JS dependency. |
| [`qweb-value`](crates/qweb-value) | A pure-Rust `Host` with Python-like values (`str()`, truthiness, …). Renders templates from Rust or WASM with JSON-like data. |
| [`qweb-inherit`](crates/qweb-inherit) | Template inheritance in both of Odoo's dialects: the server one (`odoo/tools/template_inheritance.py`, lxml model) and the browser one (`web/static/src/core/template_inheritance.js`, DOM model). Both run on its own XPath 1.0 engine. |
| [`qweb-python`](crates/qweb-python) | PyO3/maturin package `qweb_rs`: `render()` (a drop-in for `ir_qweb.render`), `Engine` (used by `ir.qweb`) and `apply_inheritance()`. |
| [`qweb-wasm`](crates/qweb-wasm) | wasm-bindgen bindings: `TemplateStore` (browser inheritance), `applyInheritanceServer` and `Renderer` (string rendering with JSON values). |

## How it works

Templates are parsed into a tree that mimics lxml (text/tail, namespaces,
comments, processing instructions). The compiler walks it in the same order as
Odoo's Python compiler and emits a compact IR, with static text merged and
static attributes pre-rendered. An interpreter then runs the IR into a single
output buffer.

Expressions are hybrid. Rust parses the Python expression subset that QWeb
allows into an AST. The host evaluates each node:
- **Python:** through the CPython object protocol, so the semantics are exactly
  Python's. Constructs such as lambdas, comprehensions and f-strings go through
  a token-for-token port of Odoo's expression rewriter and run as Python code.
- **Pure Rust (`qweb-value`):** with Python-like value semantics.

## Usage

### Python

Build the package with [maturin](https://www.maturin.rs/) (Python ≥ 3.12):

```sh
pip install maturin
cd crates/qweb-python
maturin develop -r          # or: maturin build -r, then pip install the wheel
```

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

### Rust

```rust
use qweb_value::{Value, ValueHost};

let host = ValueHost::new();
host.add_template("hello", r#"<t t-name="hello"><p t-out="name"/></t>"#);
let out = host.render("hello", [("name", Value::str("<world>"))]).unwrap();
assert_eq!(out, "<p>&lt;world&gt;</p>");
```

Implement `qweb_core::Host` to plug in your own value model, template loader
and extension hooks (custom directives, attribute processing, expression
validation).

Template inheritance on serialized XML:

```rust
let out = qweb_inherit::apply_inheritance_str(
    r#"<t t-name="x"><p>hi</p></t>"#,
    &[r#"<xpath expr="//p" position="after"><span>new</span></xpath>"#],
    qweb_inherit::Options { inherit_branding: false },
)?;
```

### JavaScript / WebAssembly

With [wasm-pack](https://rustwasm.github.io/wasm-pack/):

```sh
wasm-pack build crates/qweb-wasm --profile wasm --target web --out-dir ../../target/wasm-pack/web
# other targets: nodejs, bundler, no-modules, deno
```

```js
import init, { Renderer, applyInheritanceServer } from "./qweb_wasm.js";
await init();

const r = new Renderer();
r.addTemplate("hello", '<t t-name="hello"><p t-out="name"/></t>');
r.render("hello", JSON.stringify({ name: "<world>" }), "");  // "<p>&lt;world&gt;</p>"

applyInheritanceServer('<t t-name="x"><p>hi</p></t>',
    ['<xpath expr="//p" position="after"><span>new</span></xpath>'], false);
```

`scripts/build_wasm.sh` builds the same bindings with the system `wasm-bindgen`
(it must match the crate's pinned version, `0.2.128`).

In the browser, Odoo's Owl compiles templates to a virtual DOM and never
renders strings, so on that side WASM shares only the parsing and the
inheritance, not the rendering.

## Using it in Odoo

The Odoo integration is a patch to Odoo 20 that is not public yet. With it:
- `ir.qweb` renders through `qweb_rs` whenever the package is importable.
  `ODOO_QWEB_ENGINE=python` switches back to the Python engine.
- Field converters, widgets, groups, assets and attribute post-processing still
  call the Python `ir.qweb` methods, so module overrides keep working.
- The compile-time overrides of `website`, `mail`, `html_builder` and
  `html_editor` are ported to a new explicit hook API.
- In the web client, `@web/core/templates` applies template inheritance with
  the WASM `TemplateStore` and falls back to the JS implementation.

Without the patch, `qweb_rs.render()` and `qweb_rs.apply_inheritance()` work
on their own.

## Correctness

The reference is always Odoo's Python engine.

- **Conformance:** 158/158 cases ported from Odoo's `test_ir_qweb.py` pass
  through Python. 154 of them also pass through the pure-Rust host.
- **Rendering fuzzer:** random templates rendered by both engines, with
  0 mismatches over about 14k templates.
- **Server inheritance:** the same result as `apply_inheritance_specs` on all
  4924 view combinations of a database with website and mail, and 0 mismatches
  over 35k fuzzed cases.
- **Browser inheritance:** 0 mismatches over about 18k fuzzed cases against
  the original `template_inheritance.js` in headless Chrome. Every registered
  web client template (1390) builds identically with both backends.
- **Odoo test suites:** website, mail, mass_mailing and their test modules
  (1764 tests) give the same results on both engines. The full web unit test
  suite passes with the WASM backend.
- Website pages served by both engines are byte-identical, apart from CSRF tokens.

The port reproduces Odoo's quirks on purpose, including bugs: for example
`<span t-esc="x"/>` renders `<span></span>`, because `t-esc` no longer exists
in Odoo 20. The fuzzers also found a bug in Odoo's own inheritance code (a
nested `position="move"` can extract a temporary sentinel node); qweb-rs
matches Odoo's behaviour until it is fixed upstream.

### Known differences

- **Several new `xmlns` declarations on one element** are emitted in source
  order. Python uses set order, which depends on the hash seed.
- **Very deeply nested templates** that make Python fail with an
  `IndentationError` (CPython's limit of 100 nested blocks in the generated
  code) render fine.
- **Error messages** differ. Exception classes match (`QWebError` wrapping the
  original cause, with template, path and element).
- **Lazy content rendered twice** by `t-out` is rendered once and reused. Python
  renders it again, which only matters if rendering has side effects.

## Benchmarks

Measured with [hyperfine](https://github.com/sharkdp/hyperfine) on an AMD Ryzen 7
8845HS (Python 3.14, Node 26, release builds), with process startup subtracted.

**Rendering, compiling on every call** (`ir_qweb.render` vs `qweb_rs.render`):

| template | Python | Rust | speed-up |
|---|---:|---:|---:|
| `table_500_rows` | 10.18 ms | 0.56 ms | 18x |
| `nested_calls` | 6.72 ms | 0.30 ms | 23x |
| `static_page` | 3.11 ms | 0.12 ms | 25x |
| `small_snippet` | 0.99 ms | 0.007 ms | ~150x |

**Rendering with cached compilation** (`ir.qweb` code cache vs `qweb_rs.Engine`):

| template | Python | Rust | speed-up |
|---|---:|---:|---:|
| `table_500_rows` | 7303 µs | 537 µs | 13.6x |
| `nested_calls` | 3328 µs | 266 µs | 12.5x |
| `static_page` | 10.5 µs | 1.5 µs | 7.1x |
| `small_snippet` | 14.0 µs | 0.8 µs | 17.3x |

**Template inheritance** (4924 real view combinations, from XML to XML):

| implementation | time | vs lxml |
|---|---:|---:|
| Odoo (Python + lxml) | 0.97 s | 1x |
| `qweb_rs.apply_inheritance` | 0.46 s | 2.13x |
| WASM `applyInheritanceServer` in Node | 0.84 s | 1.16x |

**End to end**, on Odoo website pages, the gain is only 2% to 8%: routing, the
ORM, the database and the HTTP stack dominate. It matters more for heavy
templates such as large reports, mass mailings or long lists.

**In the browser**, inheritance through WASM is slightly slower than the JS
implementation (about 158 ms against 135 ms for 1390 templates), because Owl
needs a DOM and every result has to be copied back into it. The point of the
WASM backend there is to share one implementation with the server.

The benchmark drivers are in [`benches/`](benches).

## Development

```sh
cargo test --workspace                                   # unit tests + native conformance cases
cargo build -p qweb-core -p qweb-value --target wasm32-unknown-unknown   # keep the core WASM-clean

cd crates/qweb-python && maturin develop -r              # Python package
python -m pytest tests/conformance                       # differential tests (need Odoo on sys.path)
python tests/conformance/fuzz.py 3000 <seed>             # rendering fuzzer
python tests/inherit/fuzz_inherit.py 5000 <seed>         # server inheritance fuzzer
python tests/inherit_js/fuzz_browser.py 3000 <seed>      # browser inheritance fuzzer (headless Chrome)
```

The differential tests import the reference engine from an Odoo 20 source
tree (set `ODOO_PATH`) and need Odoo's Python dependencies, but no database.
New conformance cases go in `tests/conformance/cases.py`; then
`tests/conformance/gen_expected.py` regenerates the expected outputs from the
Python engine.

Miri checks (nightly, slow):

```sh
MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test -p qweb-core -p qweb-inherit -p qweb-value
```

### Platforms

The Rust code makes no assumptions about the platform: no SIMD intrinsics, no
`cfg(target_…)` code, no endianness or pointer-width assumptions. Python wheels
use the stable ABI (`abi3-py312`), so one wheel per OS and architecture covers
every Python ≥ 3.12. Keep `\n` line endings in `tests/conformance/cases.json`
and the test templates.

## License

[LGPL-3.0-only](LICENSE), like the Odoo code it is ported from.

Odoo is a trademark of Odoo S.A. This project is not affiliated with or
endorsed by Odoo S.A.
