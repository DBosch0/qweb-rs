"""Compare qweb_rs.render with the reference outputs stored in cases.json.

Run: .venv/bin/python -m pytest tests/conformance
"""
import json
import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(__file__))
from reference import make_loader, run_case, to_python  # noqa: E402

import qweb_rs  # noqa: E402

with open(os.path.join(os.path.dirname(__file__), 'cases.json'), encoding='utf-8') as f:
    CASES = json.load(f)


def render_rs(case):
    return qweb_rs.render(case['main'], to_python(case['values']), make_loader(case['templates']), **case['options'])


@pytest.mark.parametrize('case', CASES, ids=[c['name'] for c in CASES])
def test_case(case):
    got = run_case(case, render_rs)
    if 'output' in case:
        assert got == {'output': case['output']}
    else:
        assert 'error' in got, f"expected {case['error']}({case['cause']}), got output {got.get('output')!r}"
        assert (got['error'], got['cause']) == (case['error'], case['cause']), got['message']
