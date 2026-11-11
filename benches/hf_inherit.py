"""One hyperfine run: apply every captured view inheritance case.

    python benches/hf_inherit.py <engine> <cases.json>

engine: py    odoo.tools.template_inheritance.apply_inheritance_specs (lxml)
        rs    qweb_rs.apply_inheritance
        none  startup only (imports + loading the cases), to subtract
Both start from the serialized source and spec and produce a serialized
result (the Python side includes lxml parsing and serialization).
Cases come from tests/inherit/capture_views.py (QWEB_INHERIT_CASES).
"""

import json
import os
import sys

# The Odoo source tree. Without ODOO_PATH, `odoo` must be importable.
if os.environ.get("ODOO_PATH"):
    sys.path.insert(0, os.environ["ODOO_PATH"])
from lxml import etree  # noqa: E402
from odoo.tools.template_inheritance import apply_inheritance_specs  # noqa: E402

import qweb_rs  # noqa: E402

engine, path = sys.argv[1], sys.argv[2]
with open(path) as f:
    cases = json.load(f)


def py(case):
    source = etree.fromstring(case["source"])
    spec = etree.fromstring(case["spec"])
    try:
        res = apply_inheritance_specs(source, spec, inherit_branding=case["branding"])
    except Exception:  # noqa: BLE001
        return None
    return etree.tostring(res, encoding="unicode", with_tail=False)


def rs(case):
    try:
        return qweb_rs.apply_inheritance(
            case["source"], [case["spec"]], case["branding"]
        )
    except ValueError:
        return None


fn = {"py": py, "rs": rs, "none": None}[engine]
if fn:
    for case in cases:
        fn(case)
