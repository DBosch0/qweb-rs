"""Differential fuzzer: random templates rendered by the reference Python
engine and by qweb_rs; any difference in output (or exception class) is
reported with the template that produced it.

Usage: PYTHONHASHSEED=0 .venv/bin/python tests/conformance/fuzz.py [iterations] [seed]
"""
import os
import random
import sys

if os.environ.get('PYTHONHASHSEED') != '0':
    os.environ['PYTHONHASHSEED'] = '0'
    os.execv(sys.executable, [sys.executable] + sys.argv)

sys.path.insert(0, os.path.dirname(__file__))
from reference import make_loader, render_reference, require_odoo, run_case, to_python  # noqa: E402

require_odoo()

import qweb_rs  # noqa: E402

TEXTS = ['', '', ' ', '\n', '\n    ', '\n\t', 'x', '  y\n', '\n\n  ', '\n  \n', 'a&amp;b', '&lt;z&gt;', ' \n ']
TAGS = ['div', 'span', 'p', 'br', 'img', 'b']
EXPRS = [
    'a', 's', 'm', 'n', 'f', 'l', 'd', 'z', 'missing', "''", '0', '1', 'a + 1', 's * 2', "m + '<'", 'len(l)',
    'l[0]', "d['k']", "d.get('x', 'y')", 'not a', 'a and s', 'n or s', "a if f else 'no'", '[1, s]', '(a, n)',
    "{'k': a}", 'a > 2', 'a < 2 < 3', 'l', 's.upper()', "'%s!' % s", 'True', 'False', 'None', '1.5', '0.1 + 0.2',
    'v1', 'v2', 'v1 + s', 'xmlid', '0 if a else 1',
]
LOOPS = ['l', 'd', '3', 'range(2)', 'z', "'ab'", '[]', 'None', 'a']
ATT_EXPRS = ['a', 's', 'n', 'f', "''", '0', 'True', 'm', "'javascript:x'", "{'data-x': s}", "('k', 'v')", '[]']


def q(expr):
    """XML-escape an attribute value."""
    return expr.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;').replace('"', '&quot;')


class Gen:
    def __init__(self, rnd):
        self.r = rnd
        self.vars = []

    def text(self):
        return self.r.choice(TEXTS)

    def expr(self):
        if self.vars and self.r.random() < 0.3:
            v = self.r.choice(self.vars)
            return q(self.r.choice([v, f'{v}_index', f'{v}_value', f'{v}_first', f'{v}_last', f'{v}_parity', f'{v}_odd', f'{v}_size']))
        return q(self.r.choice(EXPRS))

    def attrs(self, tag):
        out = []
        for _ in range(self.r.randint(0, 3)):
            k = self.r.choice(['class', 'id', 'title', 'href', 'data-a'])
            kind = self.r.random()
            if kind < 0.3:
                out.append(f'{k}="{self.r.choice(["v", "a &amp; b", "&quot;q&quot;", ""])}"')
            elif kind < 0.6:
                out.append(f't-att-{k}="{q(self.r.choice(ATT_EXPRS))}"')
            elif kind < 0.85:
                e2 = self.expr()
                while '{' in e2:
                    e2 = self.expr()
                out.append(f't-attf-{k}="x-{{{{ {self.expr()} }}}}-#{{{e2}}}"')
            else:
                out.append(f't-att="{q(self.r.choice(ATT_EXPRS[:-1]))}"')
        # dedupe attribute names (invalid XML otherwise)
        seen, res = set(), []
        for a in out:
            name = a.split('=')[0]
            if name not in seen:
                seen.add(name)
                res.append(a)
        return res

    def children(self, depth):
        parts = [self.text()]
        n = self.r.randint(0, 4) if depth < 4 else 0
        i = 0
        while i < n:
            r = self.r.random()
            if r < 0.12:
                # if / elif / else chain
                parts.append(self.node(depth + 1, extra=f't-if="{self.expr()}"'))
                for _ in range(self.r.randint(0, 2)):
                    parts.append(self.r.choice(['', '\n  ', ' ', '<!-- c -->', '\n<!--x-->\n']))
                    parts.append(self.node(depth + 1, extra=f't-elif="{self.expr()}"'))
                if self.r.random() < 0.6:
                    parts.append(self.r.choice(['', '\n  ', ' ']))
                    parts.append(self.node(depth + 1, extra='t-else=""'))
            elif r < 0.17:
                parts.append(self.r.choice(['<!-- comment -->', '<?pi data?>']))
            else:
                parts.append(self.node(depth + 1))
            parts.append(self.text())
            i += 1
        return ''.join(parts)

    def node(self, depth, extra=''):
        tag = self.r.choice(TAGS + ['t', 't', 't'])
        r = self.r.random()
        directives = [extra] if extra else []
        pushed = False
        if r < 0.15:
            var = self.r.choice(['i', 'j', 'k'])
            directives.append(f't-foreach="{q(self.r.choice(LOOPS))}" t-as="{var}"')
            self.vars.append(var)
            pushed = True
        elif r < 0.25 and not extra:
            directives.append(f't-if="{self.expr()}"')
        if self.r.random() < 0.25:
            directives.append(f't-out="{self.expr()}"')
        elif self.r.random() < 0.1 and tag == 't' and not extra:
            name = self.r.choice(['v1', 'v2', 'z'])
            if self.r.random() < 0.5:
                directives.append(f't-set="{name}" t-value="{self.expr()}"')
                body = ''
                directives_s = ' '.join(d for d in directives if d)
                if pushed:
                    self.vars.pop()
                return f'<t {directives_s}/>'
            directives.append(f't-set="{name}"')
        elif self.r.random() < 0.1 and tag == 't' and not extra and depth < 3:
            directives.append(f't-call="{self.r.choice(["sub1", "sub2"])}"')
            if self.r.random() < 0.5:
                directives.append(f'a="{self.expr()}"')
        attrs = self.attrs(tag) if tag != 't' else []
        body = self.children(depth)
        if pushed:
            self.vars.pop()
        all_attrs = ' '.join(d for d in directives + attrs if d)
        sep = ' ' if all_attrs else ''
        if not body and self.r.random() < 0.5:
            return f'<{tag}{sep}{all_attrs}/>'
        return f'<{tag}{sep}{all_attrs}>{body}</{tag}>'


VALUES = {
    'a': 3, 's': '<s & "q">', 'm': {'$markup': '<i>m</i>'}, 'n': None, 'f': False,
    'l': [1, 'two', None], 'd': {'k': 'v', 'k2': 2}, 'z': 0,
}


def make_case(rnd):
    g = Gen(rnd)
    main = f'<t t-name="main">{g.children(0)}</t>'
    sub1 = f'<t t-name="sub1">{g.text()}<t t-out="0"/>{g.text()}<b t-out="a"/>{g.text()}</t>'
    sub2 = f'<t t-name="sub2"><div>{g.children(2)}</div></t>'
    opts = {'preserve_comments': True} if rnd.random() < 0.2 else {}
    return {'main': 'main', 'templates': {'main': main, 'sub1': sub1, 'sub2': sub2}, 'values': VALUES, 'options': opts}


def render_rs(case):
    return qweb_rs.render(case['main'], to_python(case['values']), make_loader(case['templates']), **case['options'])


def same(a, b):
    if 'output' in a or 'output' in b:
        return a == b
    return (a['error'], a['cause']) == (b['error'], b['cause'])


def main():
    iterations = int(sys.argv[1]) if len(sys.argv) > 1 else 500
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    failures = 0
    errors = 0
    skipped = 0
    causes = {}
    for it in range(iterations):
        rnd = random.Random(seed * 1_000_003 + it)
        case = make_case(rnd)
        ref = run_case(case, render_reference)
        got = run_case(case, render_rs)
        if ref.get('cause') == 'IndentationError':
            # The Python engine generates code nested deeper than CPython's
            # 100-level indentation limit: a limitation not reproduced.
            skipped += 1
            continue
        errors += 'error' in ref
        if 'error' in ref:
            causes[ref['cause']] = causes.get(ref['cause'], 0) + 1
        if not same(ref, got):
            failures += 1
            if failures <= 5:
                print(f'--- mismatch (iteration {it}, seed {seed}) ---')
                for name, src in case['templates'].items():
                    print(f'[{name}] {src!r}')
                print('options:', case['options'])
                print('reference:', ref)
                print('qweb_rs:  ', got)
    print(f'{iterations} iterations, {failures} mismatches, {errors} reference errors {causes}, '
          f'{skipped} skipped (IndentationError)')
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
