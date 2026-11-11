"""Differential fuzzing of qweb_rs.apply_inheritance against Odoo's
``odoo.tools.template_inheritance.apply_inheritance_specs``.

    python tests/inherit/fuzz_inherit.py [iterations] [seed]

Random source trees (whitespace, comments, processing instructions) and
random specs (all positions, replace modes, ``$0``, ``position="move"``,
attribute add/remove with separators, ``<data>`` groups, invalid specs).
"""
import os
import random
import sys

# The Odoo source tree. Without ODOO_PATH, `odoo` must be importable.
ODOO_PATH = os.environ.get('ODOO_PATH')
if ODOO_PATH:
    sys.path.insert(0, ODOO_PATH)

from lxml import etree  # noqa: E402

from odoo.tools.template_inheritance import apply_inheritance_specs  # noqa: E402

import qweb_rs  # noqa: E402

TAGS = ['div', 'p', 'span', 't', 'section', 'field']
CLASSES = ['a', 'b', 'c', 'o_x']
WS = ['', '', '\n', '\n    ', '\n        ', ' ', '\n  \n  ', 'x', ' y ', '\t', '\n\n']


class Gen:
    def __init__(self, rnd):
        self.r = rnd
        self.ids = 0

    def ws(self):
        return self.r.choice(WS)

    def attrs(self, tag):
        r = self.r
        a = {}
        self.ids += 1
        if r.random() < 0.7:
            a['id'] = str(self.ids)
        if r.random() < 0.4:
            a['class'] = ' '.join(r.sample(CLASSES, r.randint(1, 3)))
        if tag == 'field' or r.random() < 0.2:
            a['name'] = r.choice(['x', 'y', 'z'])
        if r.random() < 0.15:
            a['t-if'] = r.choice(['a', 'a and b', '(a) or (b)', 'b'])
        return a

    def element(self, depth):
        r = self.r
        tag = r.choice(TAGS)
        el = etree.Element(tag, self.attrs(tag))
        if r.random() < 0.6:
            el.text = self.ws()
        if depth > 0:
            for _ in range(r.randint(0, 4)):
                k = r.random()
                if k < 0.1:
                    c = etree.Comment(r.choice(['c', ' note ']))
                elif k < 0.13:
                    c = etree.ProcessingInstruction('pi', 'x')
                else:
                    c = self.element(depth - 1)
                c.tail = self.ws() if r.random() < 0.7 else None
                el.append(c)
        return el

    def source(self):
        root = self.element(3)
        root.tag = 't'
        if self.r.random() < 0.5:
            root.set('t-name', 'tmpl')
        return root

    def locator(self, source, allow_root=True):
        """A spec element locating a (usually existing) node of source."""
        r = self.r
        nodes = [n for n in source.iter() if isinstance(n.tag, str)]
        if not allow_root:
            nodes = nodes[1:] or nodes
        target = r.choice(nodes)
        k = r.random()
        if k < 0.55:
            if target.get('id') and r.random() < 0.6:
                expr = r.choice([f"//*[@id='{target.get('id')}']", f"//{target.tag}[@id='{target.get('id')}']"])
            elif target.get('class') and r.random() < 0.5:
                expr = f"//{target.tag}[hasclass('{target.get('class').split()[0]}')]"
            else:
                expr = r.choice([
                    f'//{target.tag}', f'(//{target.tag})[last()]', f'//{target.tag}[1]', '.', '/t',
                    f'//{target.tag}/..', f'//{target.tag}/following-sibling::*[1]',
                    f'//{target.tag}/preceding-sibling::*', '//nothing', "//*[text()='x']",
                    f"//{target.tag}[not(@id)]", '//t/*[2]',
                ])
            return etree.Element('xpath', {'expr': expr})
        if k < 0.75 and target.tag == 'field' and target.get('name'):
            return etree.Element('field', {'name': target.get('name')})
        attrs = {}
        if target.get('id') and r.random() < 0.8:
            attrs['id'] = target.get('id')
        return etree.Element(target.tag, attrs)

    def content(self, spec, source, depth=1):
        r = self.r
        if r.random() < 0.6:
            spec.text = self.ws()
        for _ in range(r.randint(0, 3)):
            k = r.random()
            if k < 0.12:
                c = self.locator(source, allow_root=False)
                c.set('position', 'move')
            elif k < 0.2:
                c = etree.Comment('spec')
            elif k < 0.28:
                c = etree.Element('b')
                c.text = '$0'
            else:
                c = self.element(depth)
            c.tail = self.ws() if r.random() < 0.7 else None
            spec.append(c)

    def spec(self, source):
        r = self.r
        spec = self.locator(source)
        pos = r.choice(['inside', 'after', 'before', 'replace', 'replace', 'attributes', None, 'bad'])
        if pos:
            spec.set('position', pos)
        if pos == 'replace' and r.random() < 0.4:
            spec.set('mode', r.choice(['inner', 'outer', 'bad']))
        if pos == 'attributes':
            for _ in range(r.randint(1, 3)):
                name = r.choice(['class', 't-if', 'style', 'id', 'invisible'])
                a = etree.SubElement(spec, 'attribute', {'name': name})
                k = r.random()
                if k < 0.3:
                    a.text = r.choice(['', 'v', 'a b'])
                elif name in ('t-if', 'invisible'):
                    a.set('separator', r.choice(['and', ' or ', 'xor']))
                    if r.random() < 0.6:
                        a.set('add', r.choice(['c', 'd or e']))
                    if r.random() < 0.6:
                        a.set('remove', r.choice(['a', 'b', '(a)', 'c']))
                else:
                    if r.random() < 0.7:
                        a.set('separator', r.choice([' ', ',', ';']))
                    a.set(r.choice(['add', 'remove']), r.choice(['a', 'b c', 'a, d', 'o_x']))
                a.tail = self.ws()
        else:
            self.content(spec, source)
        return spec


def run_python(source, spec, branding):
    try:
        res = apply_inheritance_specs(source, spec, inherit_branding=branding)
        return etree.tostring(res, encoding='unicode', with_tail=False), None
    except Exception as e:  # noqa: BLE001
        return None, f'{type(e).__name__}: {e}'


def run_rust(source, spec, branding):
    try:
        return qweb_rs.apply_inheritance(source, [spec], branding), None
    except BaseException as e:  # noqa: BLE001 (catches Rust panics too)
        return None, f'{type(e).__name__}: {e}'


def main():
    iterations = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    mismatches = 0
    errors = 0
    for i in range(iterations):
        rnd = random.Random(seed * 1_000_003 + i)
        g = Gen(rnd)
        source = g.source()
        if rnd.random() < 0.3:
            spec = etree.Element('data')
            for _ in range(rnd.randint(1, 3)):
                s = g.spec(source)
                s.tail = g.ws()
                spec.append(s)
        else:
            spec = g.spec(source)
        branding = rnd.random() < 0.3
        src_s = etree.tostring(source, encoding='unicode')
        spec_s = etree.tostring(spec, encoding='unicode')
        # both engines start from the same serialized trees
        py, py_err = run_python(etree.fromstring(src_s), etree.fromstring(spec_s), branding)
        rs, rs_err = run_rust(src_s, spec_s, branding)
        errors += py_err is not None
        if (py_err is None) != (rs_err is None) or py != rs:
            mismatches += 1
            if mismatches <= 5:
                print(f'--- mismatch #{i} (branding={branding})')
                print("source:", src_s[:3000])
                print('spec:  ', spec_s)
                print('python:', py or py_err)
                print('rust:  ', rs or rs_err)
    print(f'{iterations} cases, {errors} expected errors, {mismatches} mismatches')
    sys.exit(1 if mismatches else 0)


if __name__ == '__main__':
    main()
