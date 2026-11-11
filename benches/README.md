# Benchmarks

These scripts compare Odoo's Python implementation with qweb-rs. They need the
same setup as the differential tests (see `tests/README.md`): an Odoo 20 tree
in `ODOO_PATH`, its Python requirements, and `qweb_rs` built in release mode
(`cd crates/qweb-python && maturin develop -r`).

The reference engine must be the Python one. If your Odoo tree has the qweb-rs
integration, `ir_qweb.render()` uses Rust whenever `qweb_rs` is importable,
so set `ODOO_QWEB_ENGINE=python` (the render scripts do it for you through
`tests/conformance/reference.py`). Before trusting a result, check what the
"Python" side really runs.

## Quick comparison: `compare.py`

```sh
python benches/compare.py
```

It renders four templates in-process and prints a table of per-call times for
the Python engine (with its compiled-code cache) and qweb-rs:
- `table_500_rows`: a loop over 500 rows with attributes;
- `nested_calls`: nested `t-call`s;
- `static_page`: a large static page;
- `small_snippet`: a tiny template.

Use it for a fast check during development. For published numbers, use the
hyperfine drivers below.

## hyperfine drivers

Each script does one complete run in its own process, which
[hyperfine](https://github.com/sharkdp/hyperfine) times. Every driver has a
`none` mode that does all the startup work (interpreter, imports, data) but no
rendering, so you can subtract it:

```
time per call = (mean − mean of none) / N
```

Choose N so the work is much larger than the startup (about 0.5 s for the
Python scripts).

hyperfine 2.0's JSON export has no `mean` field: compute it from
`results[].measurements[].time_wall_clock.value`.

### Rendering: `hf_render.py`

```
python benches/hf_render.py <engine> <bench> <n>
```

| engine | What it runs |
|---|---|
| `py` | Odoo's `ir_qweb.render()`, which compiles on every call |
| `py-cached` | `ir.qweb` with its compiled-code cache (what Odoo does in production) |
| `rs` | `qweb_rs.render()`, the same API as `ir_qweb.render()` |
| `rs-engine` | a `qweb_rs.Engine`, which caches compiled templates |
| `none` | startup only |

`<bench>` is one of the templates of `compare.py`.

```sh
cd benches
B=table_500_rows N=300
hyperfine -N -w 1 -r 10 \
    -n none "python hf_render.py none $B 0" \
    -n py   "python hf_render.py py $B $N" \
    -n rs   "python hf_render.py rs $B $N"

hyperfine -N -w 1 -r 10 \
    -n py-cached "python hf_render.py py-cached $B $N" \
    -n rs-engine "python hf_render.py rs-engine $B $N"
```

Run it from `benches/`, because it imports `compare.py` from the current
directory. Use the full path of your venv's Python if `python` is not that
interpreter.

### Template inheritance: `hf_inherit.py` and `hf_inherit_wasm.mjs`

Both replay the inheritance cases captured from a real database. Create the
cases file first with `tests/inherit/capture_views.py` and
`QWEB_INHERIT_CASES=cases.json` (see `tests/README.md`). Each case starts from
serialized XML and ends with serialized XML, so the Python side includes lxml
parsing and serialization.

```
python benches/hf_inherit.py <py|rs|none> <cases.json>
node benches/hf_inherit_wasm.mjs <cases.json> [none]
```

```sh
hyperfine -N -w 1 -r 10 \
    -n none "python benches/hf_inherit.py none cases.json" \
    -n py   "python benches/hf_inherit.py py cases.json" \
    -n rs   "python benches/hf_inherit.py rs cases.json"

scripts/build_wasm.sh         # builds target/wasm-pkg/nodejs
hyperfine -N -w 1 -r 10 \
    -n none "node benches/hf_inherit_wasm.mjs cases.json none" \
    -n wasm "node benches/hf_inherit_wasm.mjs cases.json"
```

To benchmark another Node build, such as one made by wasm-pack, set
`QWEB_WASM_PKG` to its `qweb_wasm.js`.

### End to end: `hf_http.sh`

This driver times the same pages served by two Odoo servers, one on each
engine. Use two copies of the same database, so the servers do not share state.

```sh
# Terminal 1: Rust engine
odoo-bin -c rs.conf -d db_rs --db-filter='^db_rs$' --http-port 18069

# Terminal 2: Python engine
ODOO_QWEB_ENGINE=python odoo-bin -c py.conf -d db_py --db-filter='^db_py$' --http-port 18169

# hf_http.sh <rust base url> <python base url> [paths...]
benches/hf_http.sh http://127.0.0.1:18069 http://127.0.0.1:18169 / /contactus /web/login
```

Without paths, it times `/`, `/contactus`, `/website/info` and `/web/login`
(these need the `website` module). Run the servers with `workers = 0`. Each
path gets 20 warm-up requests and 200 timed ones. Stop the servers afterwards.

Expect only a few percent here: routing, the ORM, the database and the HTTP
stack dominate a page.

## Recording results

Write the date and the machine (CPU, Python and Node versions, CPU governor)
next to any numbers you record. Close other workloads, and use a fixed
`performance` governor if you can.
