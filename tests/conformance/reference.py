"""Helpers to run the reference Python engine (Odoo's ir_qweb.render)."""
import logging
import os
import sys
import types

# The reference is the Python engine: Odoo branches with the qweb_rs
# integration render with Rust when qweb_rs is importable.
os.environ['ODOO_QWEB_ENGINE'] = 'python'

# The Odoo source tree (only needed for the reference renders, not for the
# tests against cases.json). Without ODOO_PATH, `odoo` must be importable.
ODOO_PATH = os.environ.get('ODOO_PATH')
if ODOO_PATH and ODOO_PATH not in sys.path:
    sys.path.insert(0, ODOO_PATH)

from lxml import etree  # noqa: E402
from markupsafe import Markup  # noqa: E402

logging.getLogger('odoo').setLevel(logging.CRITICAL)


def to_python(v):
    if isinstance(v, dict):
        if len(v) == 1:
            (k, inner), = v.items()
            if k == '$markup':
                return Markup(inner)
            if k == '$tuple':
                return tuple(to_python(x) for x in inner)
            if k == '$object':
                return types.SimpleNamespace(**{a: to_python(b) for a, b in inner.items()})
            if k == '$float':
                return float(inner)
        return {k: to_python(x) for k, x in v.items()}
    if isinstance(v, list):
        return [to_python(x) for x in v]
    return v


def make_loader(templates):
    def load(name):
        return etree.fromstring(templates[str(name)]), name
    return load


def require_odoo():
    """Exit unless Odoo is importable (run_case would record the ImportError as a result)."""
    try:
        import odoo.addons.base.models.ir_qweb  # noqa: F401
    except ImportError as e:
        sys.exit(f"cannot import Odoo ({e}): set ODOO_PATH to the Odoo source tree")


def render_reference(case):
    from odoo.addons.base.models.ir_qweb import render
    values = to_python(case['values'])
    return render(case['main'], values, make_loader(case['templates']), **case['options'])


def run_case(case, renderer):
    """Return ('ok', output) or ('error', class name, cause class name)."""
    try:
        return {'output': str(renderer(case))}
    except Exception as e:  # noqa: BLE001
        cause = e.__cause__ or e.__context__
        return {
            'error': type(e).__name__,
            'cause': type(cause).__name__ if cause else None,
            'message': str(e).split('\n')[0][:300],
        }
