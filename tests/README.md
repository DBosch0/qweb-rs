# Tests

All the tests here are differential: they run the same input through Odoo's
original implementation and through qweb-rs, and compare the results. The
reference is always Odoo's Python (or JS) code.

The Rust unit tests live in the crates and run with `cargo test --workspace`.
That command also runs the native conformance cases (`cases.json`) through the
pure-Rust host (`crates/qweb-value/tests/conformance.rs`).

## Setup

You need:
- **An Odoo 20 source tree.** Point `ODOO_PATH` at it (the directory that
  contains `odoo-bin`).
- **A Python ≥ 3.12 environment** with Odoo's requirements
  (`pip install -r $ODOO_PATH/requirements.txt`), plus `pytest` and `maturin`.
- **The `qweb_rs` package** installed in that environment:

  ```sh
  cd crates/qweb-python && maturin develop -r
  ```

Rebuild the package after every Rust change, otherwise the tests run the
old build.

No database is needed, except for `inherit/capture_views.py` and `odoo/`.

If your Odoo tree has the qweb-rs integration, `ir_qweb.render()` uses Rust
whenever `qweb_rs` is importable. The scripts set `ODOO_QWEB_ENGINE=python`
for the reference. Set it yourself in any other comparison.

## `conformance/`: rendering

| File | What it is |
|---|---|
| `cases.py` | The conformance cases, mostly ported from Odoo's `test_ir_qweb.py`. |
| `cases.json` | The cases with their expected output, generated from the Python engine. |
| `reference.py` | Runs Odoo's `ir_qweb.render()` (database-less). |
| `gen_expected.py` | Regenerates `cases.json`. |
| `test_conformance.py` | Renders every case with `qweb_rs.render()` and compares with `cases.json`. |
| `test_bridge.py` | Custom directives through `qweb_rs.Bridge` (compile hooks and the `Element`/`Compiler` proxies). |
| `fuzz.py` | Random templates rendered by both engines. |

```sh
# Run all the cases and the bridge tests
python -m pytest tests/conformance

# After adding or changing cases in cases.py, regenerate the expected outputs
python tests/conformance/gen_expected.py

# Fuzz: fuzz.py [iterations] [seed]   (defaults: 500, 0)
python tests/conformance/fuzz.py 3000 1
```

Each case has templates, values and render options. Values are JSON-like.
Special forms such as `{"$markup": "..."}`, `{"$tuple": [...]}`,
`{"$object": {...}}` and `{"$float": "..."}` stand for the Python objects
(see `to_python` in `reference.py`). Mark a case with `native=False` when the
pure-Rust host cannot run it (lambdas, comprehensions, some string methods).

`gen_expected.py` and `fuzz.py` re-run themselves with `PYTHONHASHSEED=0`.
Python emits new `xmlns` declarations in set order, and seed 0 gives source
order, which is what qweb-rs uses.

The fuzzer prints each mismatch with its templates, options and both outputs,
then a summary. Its exit status is 1 if anything differs. Errors count as
equal when the exception classes match. The messages may differ.

## `inherit/`: template inheritance, server dialect

These scripts compare `qweb_rs.apply_inheritance` with Odoo's
`odoo.tools.template_inheritance.apply_inheritance_specs` (lxml).

```sh
# Fuzz: fuzz_inherit.py [iterations] [seed]   (defaults: 2000, 0)
python tests/inherit/fuzz_inherit.py 5000 1
```

`capture_views.py` replays every real view combination of a database. It
runs inside an Odoo shell, records every `apply_inheritance_specs` call made
while combining all the views (with and without branding), and replays each
one with Rust:

```sh
cd $ODOO_PATH
QWEB_INHERIT_CASES=/tmp/cases.json QWEB_INHERIT_FAILURES=/tmp/failures.json \
    python odoo-bin shell -c <odoo.conf> -d <db> --no-http < /path/to/qweb-rs/tests/inherit/capture_views.py
```

- `QWEB_INHERIT_CASES` (optional) saves the captured cases as JSON. The
  inheritance benchmarks use that file (see `benches/README.md`).
- `QWEB_INHERIT_FAILURES` (optional) saves the mismatches.

A database with more modules installed gives more cases. One with website,
mail and mass_mailing gives about 4900.

## `inherit_js/`: template inheritance, browser dialect

`fuzz_browser.py` compares the WASM `TemplateStore` with Odoo's
`web/static/src/core/template_inheritance.js` in headless Chrome. It builds a
self-contained HTML page (the original JS module, the wasm-bindgen glue, the
`.wasm` file in base64 and the fuzzer), runs it, and reports the mismatches.

```sh
scripts/build_wasm.sh            # builds target/wasm-pkg/no-modules, which the fuzzer loads

# fuzz_browser.py [iterations] [seed]   (defaults: 2000, 0)
CHROME=chromium python tests/inherit_js/fuzz_browser.py 3000 1
```

- `CHROME` is the Chrome or Chromium binary (default `google-chrome-stable`).
- `ODOO_PATH` must contain `addons/web`.

## `odoo/`: Odoo's own test suites

`run_tests.sh` runs Odoo test tags against a database, then prints the
failures, errors and timings from the log. It is a thin wrapper around
`odoo-bin --test-tags ... --stop-after-init`. It needs a PostgreSQL server, an
Odoo configuration file, and an Odoo tree with the qweb-rs integration.

```sh
# run_tests.sh <db> <test-tags> [log-name]
ODOO=$ODOO_PATH CONF=<odoo.conf> PGDIR=<log dir> PY=<python> \
    tests/odoo/run_tests.sh <db> "$(cat tests/odoo/base_tags.txt)"

# Same suite with the Python engine, for comparison
ODOO_QWEB_ENGINE=python ODOO=$ODOO_PATH CONF=<odoo.conf> \
    tests/odoo/run_tests.sh <db> "$(cat tests/odoo/base_tags.txt)" base_py
```

| Variable | Default | Meaning |
|---|---|---|
| `ODOO` | `~/Documents/odoo_src/odoo` | Odoo source tree. |
| `PGDIR` | `~/.cache/qweb-rs-pg` | Where the log (`<log-name>.log`) is written. |
| `CONF` | `$PGDIR/odoo.conf` | Odoo configuration file. |
| `PY` | `.venv/bin/python` of this repository | Python interpreter. |
| `ODOO_QWEB_ENGINE` | unset (Rust) | `python` runs the reference engine. |

`base_tags.txt` lists the QWeb test classes of the `base` module. Run both
engines on the same database and compare the two failure lists: they should
be identical.
