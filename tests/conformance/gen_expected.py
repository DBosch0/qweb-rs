"""Run with PYTHONHASHSEED=0: Python emits new xmlns declarations in set order
(hash-randomized); seed 0 gives source order, which qweb-rs uses.

Render every case with the reference engine and store expected results in cases.json."""
import json
import os
import sys

if os.environ.get('PYTHONHASHSEED') != '0':
    os.environ['PYTHONHASHSEED'] = '0'
    os.execv(sys.executable, [sys.executable] + sys.argv)

sys.path.insert(0, os.path.dirname(__file__))
from cases import CASES  # noqa: E402
from reference import render_reference, run_case  # noqa: E402

out = []
for case in CASES:
    res = run_case(case, render_reference)
    out.append({**case, **res})
path = os.path.join(os.path.dirname(__file__), 'cases.json')
with open(path, 'w') as f:
    json.dump(out, f, indent=1, ensure_ascii=False)
print(f'{len(out)} cases written to {path}; errors: {sum("error" in c for c in out)}')
