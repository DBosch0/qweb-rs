"""Differential test of qweb_rs.apply_inheritance against Odoo's
apply_inheritance_specs on every real view combination of a database.

Run inside an Odoo shell:

    odoo-bin shell -c <conf> -d <db> --no-http < tests/inherit/capture_views.py

Each call of ``apply_inheritance_specs`` made while combining all the views
(with and without branding) is replayed with the Rust implementation and the
serialized results are compared. Cases are also saved to
``$QWEB_INHERIT_CASES`` (JSON) when set, for offline replays/benchmarks.
"""
import json
import os
import time

from lxml import etree

import odoo.addons.base.models.ir_ui_view as V
import qweb_rs

orig = V.apply_inheritance_specs
cases = []


def wrapped(source, specs_tree, inherit_branding=False, pre_locate=None):
    src = etree.tostring(source, encoding='unicode', with_tail=False)
    spec = etree.tostring(specs_tree, encoding='unicode', with_tail=False)
    try:
        res = orig(source, specs_tree, inherit_branding=inherit_branding, pre_locate=pre_locate)
    except Exception as e:  # noqa: BLE001
        cases.append({'source': src, 'spec': spec, 'branding': bool(inherit_branding), 'error': f'{type(e).__name__}: {e}'})
        raise
    cases.append({'source': src, 'spec': spec, 'branding': bool(inherit_branding),
                  'result': etree.tostring(res, encoding='unicode', with_tail=False)})
    return res


V.apply_inheritance_specs = wrapped
try:
    Views = env['ir.ui.view']  # noqa: F821
    roots = Views.with_context(active_test=False).search([])
    for branding in (False, True):
        for view in roots.with_context(inherit_branding=branding):
            try:
                view._get_combined_arch()
            except Exception:  # noqa: BLE001, S110
                pass
finally:
    V.apply_inheritance_specs = orig
    env.cr.rollback()  # noqa: F821

ok = fail = 0
failures = []
t_rs = 0.0
for case in cases:
    t0 = time.perf_counter()
    try:
        got = qweb_rs.apply_inheritance(case['source'], [case['spec']], case['branding'])
        err = None
    except ValueError as e:
        got, err = None, str(e)
    t_rs += time.perf_counter() - t0
    if 'error' in case:
        good = err is not None
    else:
        good = got == case['result']
    if good:
        ok += 1
    else:
        fail += 1
        failures.append({**case, 'got': got, 'got_error': err})

print(f'inheritance cases: {len(cases)}  ok: {ok}  mismatches: {fail}  rust time: {t_rs:.3f}s')
out = os.environ.get('QWEB_INHERIT_CASES')
if out:
    with open(out, 'w', encoding='utf-8') as f:
        json.dump(cases, f)
with open(os.environ.get('QWEB_INHERIT_FAILURES', os.devnull), 'w', encoding='utf-8') as f:
    json.dump(failures, f, indent=1)
