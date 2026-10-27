"""Conformance cases rendered with both the reference Python engine
(`odoo.addons.base.models.ir_qweb.render`) and qweb-rs.

Each case: templates (dict name -> xml, or a str for the single template
"main"), values (JSON-like, see `to_python`), options (render kwargs).
`native=False` marks cases needing Python-only features (lambda,
comprehension, str methods not implemented by the pure-Rust host, ...).
"""

CASES = []


def case(name, templates, values=None, options=None, main='main', native=True):
    if isinstance(templates, str):
        templates = {main: templates}
    CASES.append({
        'name': name,
        'templates': templates,
        'main': main,
        'values': values or {},
        'options': options or {},
        'native': native,
    })


M = lambda s: {'$markup': s}  # noqa: E731
T = lambda *items: {'$tuple': list(items)}  # noqa: E731
O = lambda **fields: {'$object': fields}  # noqa: E731

# --------------------------------------------------------------------------
# basics / static
# --------------------------------------------------------------------------
case('trivial', '<t t-name="main"><div>Hello</div></t>')
case('static_attrs', '<t t-name="main"><div class="a &amp; b" id="x" data-x="&quot;q&apos;"><span/><br/><img src="x"/></div></t>')
case('static_text_entities', '<t t-name="main"><p>&amp;lt; &gt; &#233; &#x1F600; <![CDATA[<raw & stuff>]]></p></t>')
case('root_not_t', '<div t-name="main" class="root"><p>x</p></div>')
case('root_whitespace', '<t t-name="main">\n\n\n    <div>a</div>\n    <div>b</div>\n</t>')
case('root_whitespace2', '<t t-name="main">\n  \n\t\n    <div>a</div>\n\n</t>')
case('void_elements', '<t t-name="main"><br/><hr></hr><input value="1"/><meta/><link/><div/><span></span></t>')
case('comments_dropped', '<t t-name="main"><div><!-- c -->a<!--d-->b</div></t>')
case('comments_kept', '<t t-name="main"><div><!-- c -->a<!--d-->b<?pi x y?></div></t>', options={'preserve_comments': True})
case('translate_suffix_static', '<t t-name="main"><img alt.translate="Alt" title="t" title.translate="T2"/></t>')
case('unknown_directive', '<t t-name="main"><span t-esc="1">x</span><span t-raw="2"/><span t-value="3"/></t>')
case('special_attrs', '<t t-name="main"><div t-translation="off" t-ignore="1" name="n" id="i">x</div></t>')
case('t_tag_alone', '<t t-name="main"><t>a</t><t><t>b</t></t></t>')
case('nested_static', '<t t-name="main"><ul><li>1</li><li>2</li></ul>\n<p>  spaced  </p></t>')

# --------------------------------------------------------------------------
# t-out
# --------------------------------------------------------------------------
case('out_basic', '<t t-name="main"><div t-out="a">Default</div></t>', {'a': 1})
case('out_escape', '<t t-name="main"><div t-out="a"/></t>', {'a': '<b>"x" & \'y\'</b>'})
case('out_markup', '<t t-name="main"><div t-out="a"/></t>', {'a': M('<b>bold</b>')})
case('out_none_default', '<t t-name="main"><div t-out="a">Default</div><span t-out="b"/>|</t>', {'a': None, 'b': None})
case('out_false', '<t t-name="main"><div t-out="a">Default</div><span t-out="a"/>|</t>', {'a': False})
case('out_zero_empty', '<t t-name="main"><div t-out="a">D</div><div t-out="b">D</div></t>', {'a': 0, 'b': ''})
case('out_missing', '<t t-name="main"><div t-out="missing">D</div></t>')
case('out_on_t', '<t t-name="main"><t t-out="a"/><t t-out="b">def</t></t>', {'a': 'x<'})
case('out_types', '<t t-name="main"><i t-out="1.5"/><i t-out="1e20"/><i t-out="0.1+0.2"/><i t-out="True"/><i t-out="[1, \'a\']"/><i t-out="(1,)"/><i t-out="{\'a\': 1}"/><i t-out="10 / 4"/><i t-out="7 // 2"/><i t-out="-7 % 3"/><i t-out="2 ** 10"/></t>')
case('out_default_value', '''<t t-name="main">
                <span rows="10" t-out="a">
                    DEFAULT
                    <t t-out="'Text'" />
                </span>
            </t>''', {'a': 'Hello'})
case('out_default_value2', '''<t t-name="main">
                <span rows="10" t-out="a">
                    DEFAULT
                    <t t-out="'Text'" />
                </span>
            </t>''')
case('out_empty_expr', '<t t-name="main"><span t-out=""/></t>')
case('out_dynamic_attrs', '<t t-name="main"><span class="c" t-att-id="i" t-out="v"/></t>', {'i': 3, 'v': 'x'})
case('out_dynamic_attrs_default', '<t t-name="main"><span class="c" t-att-id="i" t-out="v">def</span><b t-att-x="1" t-out="v"/>|</t>', {'i': 3, 'v': None})

# --------------------------------------------------------------------------
# t-if / t-elif / t-else
# --------------------------------------------------------------------------
case('if_basic', '<t t-name="main"><div t-if="a">yes</div><div t-if="not a">no</div></t>', {'a': True})
case('if_elif_else', '''<t t-name="main">
    <t t-foreach="[1, 2, 3]" t-as="i">
        <p t-if="i == 1">one</p>
        <p t-elif="i == 2">two</p>
        <p t-else="">other</p>
    </t>
</t>''')
case('if_spaces', '''<t t-name="main">
                <div>
                    0
                    <t>1</t>
                    <t t-if="True">2</t>
                    <t>3</t>
                    4
                    <t>5</t>
                    6
                    <t t-if="True">7</t>
                    8
                    <t t-if="False">9</t>
                    10
                    <t t-if="False">11</t>
                    <t t-else="">12</t>
                    13
                </div>
            </t>''')
case('if_comment', '''<t t-name="main">
                <div>
                    <!-- comment 0 -->
                    0
                    <div>1</div>
                    <!-- comment 1 -->
                    <div t-if="True">2 (t-if)</div>
                    <!-- comment 2 -->
                    <div t-else="">3 (t-else)</div>
                    <!-- comment 3 -->
                    <div>4</div>
                    <!-- comment 4 -->
                    <div t-if="False">5 (t-if)</div>
                    <!-- comment 5 -->
                    <div t-else="">6 (t-else)</div>
                    <!-- comment 6 -->
                    <div>7</div>
                </div>
            </t>''')
case('if_comment_preserved', '''<t t-name="main">
    <div t-if="False">a</div>
    <!-- c1 -->
    <div t-else="">b</div>
    tail
</t>''', options={'preserve_comments': True})
case('if_t_text_newline', '<t t-name="main">a\n  <t t-if="x">\n    yes\n  </t>\n  b</t>', {'x': True})
case('if_t_text_newline_false', '<t t-name="main">a\n  <t t-if="x">\n    yes\n  </t>\n  b</t>', {'x': False})
case('if_dollar_quirk', '<t t-name="main">a\n  \n<t t-if="1">b</t></t>')
case('if_else_on_same_node', '''<t t-name="main"><t t-set="check" t-value="1"/>
<section t-if="False">10</section>
<span t-elif="check == 1" t-foreach="range(3)" t-as="check" t-out="check"/>

<section t-if="False">10</section>
<div t-else="" t-if="check == 1" t-foreach="range(3)" t-as="check" t-out="check"/>
</t>''')
case('if_else_text_between', '<t t-name="main"><t t-if="a">x</t> y <t t-else="">z</t></t>', {'a': 1})
case('else_without_if', '<t t-name="main"><t t-else="">z</t></t>')
case('if_empty_expr', '<t t-name="main"><t t-if="">x</t>y</t>')
case('if_space_expr', '<t t-name="main"><t t-if=" ">x</t></t>')
case('if_chain_many', '''<t t-name="main"><t t-foreach="range(5)" t-as="n">
  <b t-if="n == 0">zero</b>
  <b t-elif="n == 1">one</b>
  <!-- c -->
  <b t-elif="n == 2">two</b>
  <b t-elif="n == 3">three</b>
  <b t-else="">many</b>
</t></t>''')
case('if_and_or_values', '<t t-name="main"><i t-out="a and b"/><i t-out="a or b"/><i t-out="b or c"/><i t-out="not a"/><i t-out="1 if a else 2"/><i t-out="1 &lt; 2 &lt; 3"/><i t-out="3 &gt; 2 == 2"/><i t-out="a is None"/><i t-out="c is not None"/><i t-out="\'x\' in \'xyz\'"/><i t-out="2 not in [1,2]"/></t>', {'a': 'A', 'b': '', 'c': 0})

# --------------------------------------------------------------------------
# t-foreach
# --------------------------------------------------------------------------
case('foreach_list', '<t t-name="main"><t t-foreach="items" t-as="i"><span t-out="i"/>,</t></t>', {'items': [1, 'a', 2.5]})
case('foreach_vars', '''<t t-name="main"><t t-foreach="items" t-as="i">
    [<t t-out="i"/>|<t t-out="i_index"/>|<t t-out="i_value"/>|<t t-out="i_size"/>|<t t-out="i_first"/>|<t t-out="i_last"/>|<t t-out="i_odd"/>|<t t-out="i_even"/>|<t t-out="i_parity"/>]
</t></t>''', {'items': ['a', 'b', 'c']})
case('foreach_dict', '<t t-name="main"><t t-foreach="d" t-as="k"><t t-out="k"/>=<t t-out="k_value"/>;</t></t>', {'d': {'a': 1, 'b': 2}})
case('foreach_int', '<t t-name="main"><t t-foreach="3" t-as="i"><t t-out="i"/></t>|<t t-foreach="n" t-as="j"><t t-out="j"/><t t-out="j_last"/></t></t>', {'n': 2})
case('foreach_empty', '<t t-name="main"><t t-foreach="[]" t-as="i">x</t><t t-foreach="None" t-as="i">y</t><t t-foreach="missing" t-as="i">z</t>|</t>')
case('foreach_leak', '<t t-name="main"><t t-foreach="[1,2]" t-as="i"/><t t-out="i"/>|<t t-out="i_index"/></t>')
case('foreach_element', '<t t-name="main"><ul><li t-foreach="items" t-as="item" t-att-class="item_parity" t-out="item"/></ul></t>', {'items': ['a', 'b', 'c']})
case('foreach_nested', '''<t t-name="main">
    <table>
        <tr t-foreach="rows" t-as="row">
            <td t-foreach="row" t-as="cell"><t t-out="cell"/></td>
        </tr>
    </table>
</t>''', {'rows': [[1, 2], [3, 4]]})
case('foreach_space_t', '''<t t-name="main">
    <div>
        <t t-foreach="[1, 2]" t-as="x">
            <span t-out="x"/>
        </t>
    </div>
</t>''')
case('foreach_str', '<t t-name="main"><t t-foreach="\'abc\'" t-as="c"><t t-out="c"/>.</t></t>')
case('foreach_tuple_items', '<t t-name="main"><t t-foreach="[(1, \'a\'), (2, \'b\')]" t-as="p"><t t-out="p[0]"/><t t-out="p[1]"/></t></t>')
case('foreach_bad_as', '<t t-name="main"><t t-foreach="[1]" t-as="a-b"/></t>')
case('foreach_missing_as', '<t t-name="main"><t t-foreach="[1]"/></t>')
case('as_without_foreach', '<t t-name="main"><t t-as="x"/></t>')
case('foreach_if', '<t t-name="main"><t t-foreach="range(6)" t-as="i" t-if="i % 2"><t t-out="i"/></t></t>')

# --------------------------------------------------------------------------
# t-set
# --------------------------------------------------------------------------
case('set_value', '<t t-name="main"><t t-set="a" t-value="1 + 2"/><t t-out="a"/></t>')
case('set_valuef', '<t t-name="main"><t t-set="a" t-valuef="x-{{b}}-#{c}"/><t t-out="a"/></t>', {'b': '<B>', 'c': None})
case('set_body', '<t t-name="main"><t t-set="a"><b>bold</b> <t t-out="x"/></t><div t-out="a"/></t>', {'x': '<y>'})
case('set_empty_body', '<t t-name="main"><t t-set="a"/><t t-set="b"></t>[<t t-out="a"/>][<t t-out="b"/>]</t>')
case('set_body_spaces', '''<t t-name="main">
    <t t-set="a">
        <span>x</span>
    </t>
    <div t-out="a"/>
</t>''')
case('set_value_empty', '<t t-name="main"><t t-set="a" t-value=""/><t t-out="a">none</t></t>')
case('set_dict', '<t t-name="main"><t t-set="{\'a\': 1, \'b\': 2}"/><t t-out="a"/><t t-out="b"/></t>')
case('set_invalid_name', '<t t-name="main"><t t-set="a-b" t-value="1"/></t>')
case('set_dunder', '<t t-name="main"><t t-set="__a" t-value="1"/></t>')
case('set_zero_value', '<t t-name="main"><t t-set="0" t-value="1"/></t>')
case('set_lazy_snapshot', '<t t-name="main"><t t-set="x" t-value="1"/><t t-set="b"><t t-out="x"/></t><t t-set="x" t-value="2"/><t t-out="b"/><t t-out="x"/></t>')
case('set_lazy_inner_set', '<t t-name="main"><t t-set="b"><t t-set="y" t-value="5"/><t t-out="y"/></t><t t-out="b"/>|<t t-out="y"/></t>')
case('set_body_in_condition', '<t t-name="main"><t t-set="abc"> <span a="b"> a </span> </t><div t-if="abc">123</div><div t-if="not abc">456</div></t>')
case('set_lazy_concat', '''<t t-name="main">
                <t t-set="abc"> <span a="b"> a </span> </t>
                <div t-out="(v + abc)">123</div>
                <div t-out="(abc + v)">123</div>
            </t>''', {'v': '"yes"'})
case('set_lazy_percent', '''<t t-name="main">
                <t t-set="final_message">Powered by %s%s</t>
                <div t-out="final_message % (a, b and ('-%s' % b) or '')"/>
            </t>''', {'a': 1, 'b': 2})
case('set_lazy_percent2', '''<t t-name="main">
                <t t-set="toto">Toto %s</t>
                <t t-set="abc"> <span a="b"> [%s , %s] </span> </t>
                <div t-out="(abc % (add_abc, toto)) % 5">123</div>
            </t>''', {'add_abc': '"yes"'})
case('set_lazy_in', '''<t t-name="main">
                <t t-set="val"><b>TOTO %s</b></t>
                <t t-if="'TOTO' in val">if 'TOTO' in val</t>
                <t t-if="'>' in val">if > in val</t>
                <t t-if="'&lt;b>' in val">if tag in val</t>
                <a t-att-help="val % 1"/>
            </t>''')
case('set_lazy_methods', '''<t t-name="main">
                <t t-set="abc"> <span a="b"> a </span> </t>
                <div t-out="(abc.strip() + v)">123</div>
                <t t-set="val"><b>TOTO %s</b></t>
                <a t-out="val.replace('T', '_')"/>
            </t>''', {'v': '"yes"'})
case('set_on_element', '<t t-name="main"><div t-set="a" t-value="3">x</div><t t-out="a"/></t>')
case('set_and_out', '<t t-name="main"><t t-set="a" t-value="1" t-out="a"/></t>')

# --------------------------------------------------------------------------
# attributes
# --------------------------------------------------------------------------
case('att_basic', '<t t-name="main"><div t-att-a="1" t-att-b="None" t-att-c="False" t-att-d="\'\'" t-att-e="0" t-att-f="True" t-att-g="[]" t-att-h="\'x\'"/></t>')
case('att_escaping', '<t t-name="main"><div t-att-foo="bar"/><div t-att-m="m"/></t>', {'bar': '"<>&\'', 'm': M('<b>')})
case('att_order', '<t t-name="main"><div b="1" t-att-a="2" class="c" t-attf-z="z" t-att="{\'class\': \'d\', \'y\': 5}" t-att-class="\'e\'"/></t>')
case('att_dict', '<t t-name="main"><div t-att="{\'a\': 1, \'b\': None, \'c\': \'\'}"/></t>')
case('att_pair', '<t t-name="main"><div t-att="[\'a\', \'b\']"/><div t-att="(\'c\', 1)"/></t>')
case('att_pairs', '<t t-name="main"><div t-att="[(\'a\', 1), (\'b\', 2)]"/></t>')
case('att_other', '<t t-name="main"><div t-att="None"/><div t-att="5"/></t>')
case('attf', '<t t-name="main"><div t-attf-class="a {{b}} #{c} {{d}}" t-attf-e="plain"/></t>', {'b': 'B', 'c': '<C>', 'd': None})
case('attf_percent', '<t t-name="main"><div t-attf-class="100% {{b}}%s"/></t>', {'b': 'B'})
case('attf_markup', '<t t-name="main"><div t-attf-x="{{m}}"/></t>', {'m': M('<b>')})
case('attf_nested_braces', '<t t-name="main"><div t-attf-x="{{ {\'a\': 1}[\'a\'] }}"/></t>')
case('att_href_js', '<t t-name="main"><a t-att-href="u">x</a><a t-attf-href="{{u}}">y</a><a href="javascript:alert(2)">static</a></t>', {'u': 'javascript:alert(1)'})
case('att_href_js_variants', '<t t-name="main"><t t-foreach="urls" t-as="u"><a t-att-href="u"/></t></t>',
     {'urls': ['JaVaScRiPt:x', ' java\tscript:x', 'java%73cript:x', 'javascript:history.back()', 'javascript:window.history.back()', '/ok', '', 'javascript:history.back();x', 'jav+ascript:x']})
case('att_on_t', '<t t-name="main"><t t-att-x="1">a</t><t class="c" t-att-y="2" t-out="\'b\'"/></t>')
case('att_root_t', '<t t-name="main" t-att-x="1">a</t>')
case('att_translate', '<t t-name="main"><div t-attf-title.translate="T {{1}}" t-att-x="1"/></t>')
case('att_no_propagation', '<t t-name="main"><div t-att-a="1"><span t-out="2"/><p>x</p></div><p t-att-b="1"/></t>')
case('att_names_escaped', '<t t-name="main"><div t-att="{\'a&lt;b\': 1}"/></t>')
case('att_void', '<t t-name="main"><img t-att-src="s"/><br t-att-class="\'x\'"/></t>', {'s': 'a.png'})
case('att_int_key', '<t t-name="main"><div t-att="{1: 2}"/></t>')
case('att_empty_dict_then_child', '<t t-name="main"><div t-att="{}"><span t-att-a="1">x</span></div></t>')

# --------------------------------------------------------------------------
# t-call
# --------------------------------------------------------------------------
case('call_basic', {
    'main': '<t t-name="main"><div><t t-call="sub"/></div></t>',
    'sub': '<t t-name="sub"><span>sub</span></t>',
})
case('call_body', {
    'main': '<t t-name="main"><t t-call="sub"><b>body</b></t></t>',
    'sub': '<t t-name="sub"><div><t t-out="0"/></div></t>',
})
case('call_args', {
    'main': '<t t-name="main"><t t-call="sub" a="1 + 1" b.f="x-{{c}}" t-args="{\'d\': 4}"/><t t-out="a"/></t>',
    'sub': '<t t-name="sub"><t t-out="a"/>,<t t-out="b"/>,<t t-out="d"/>,<t t-out="c"/></t>',
}, {'c': 'C'})
case('call_scope', {
    'main': '<t t-name="main"><t t-set="x" t-value="1"/><t t-call="sub"/><t t-out="x"/><t t-out="y"/></t>',
    'sub': '<t t-name="sub"><t t-set="x" t-value="2"/><t t-set="y" t-value="3"/><t t-out="x"/></t>',
})
case('call_set_in_body', {
    'main': '<t t-name="main"><t t-call="sub"><t t-set="v" t-value="\'from body\'"/></t></t>',
    'sub': '<t t-name="sub"><t t-out="v"/>|<t t-out="0"/>|<t t-out="v"/></t>',
})
case('call_dynamic', {
    'main': '<t t-name="main"><t t-call="sub_{{n}}"/></t>',
    'sub_1': '<t t-name="sub_1">one</t>',
}, {'n': 1})
case('call_numeric', {
    'main': '<t t-name="main"><t t-call="42"/></t>',
    '42': '<t t-name="x42">forty-two</t>',
})
case('call_nested', {
    'main': '<t t-name="main"><t t-call="a"><t t-call="b">inner</t></t></t>',
    'a': '<t t-name="a">[a:<t t-out="0"/>]</t>',
    'b': '<t t-name="b">[b:<t t-out="0"/>]</t>',
})
case('call_zero_twice', {
    'main': '<t t-name="main"><t t-call="sub"><i t-out="n"/></t></t>',
    'sub': '<t t-name="sub"><t t-set="n" t-value="2"/><t t-out="0"/><t t-out="0"/></t>',
}, {'n': 1})
case('call_recursion', {
    'main': '<t t-name="main"><t t-call="main"/></t>',
})
case('call_recursive_counter', {
    'main': '<t t-name="main"><t t-if="n &gt; 0"><t t-out="n"/><t t-call="main"><t t-set="n" t-value="n - 1"/></t></t></t>',
}, {'n': 5})
case('call_not_on_t', {'main': '<t t-name="main"><div t-call="sub"/></t>', 'sub': '<t t-name="sub"/>'})
case('call_space', {
    'main': '''<t t-name="main">
    <div>
        <t t-call="sub"/>
        <t t-call="sub">
            <b>x</b>
        </t>
    </div>
</t>''',
    'sub': '''<t t-name="sub">
    <p>sub</p>
    <t t-out="0"/>
</t>''',
})
case('call_xmlid', {
    'main': '<t t-name="main"><t t-out="xmlid"/>|<t t-call="sub"/></t>',
    'sub': '<t t-name="sub"><t t-out="xmlid"/></t>',
})
case('call_options_comments', {
    'main': '<t t-name="main"><t t-call="sub" t-options="{\'preserve_comments\': True}"/></t>',
    'sub': '<t t-name="sub"><!-- kept --></t>',
})
case('call_foreach', {
    'main': '<t t-name="main"><t t-foreach="[1, 2]" t-as="i"><t t-call="sub"/></t></t>',
    'sub': '<t t-name="sub">[<t t-out="i"/>]</t>',
})
case('call_percent_attr', {
    'main': '<t t-name="main"><t t-call="sub" a.f="100%"/></t>',
    'sub': '<t t-name="sub"><t t-out="a"/></t>',
})
case('out_zero_outside_call', '<t t-name="main">[<t t-out="0"/>]<div t-out="0"/></t>')

# --------------------------------------------------------------------------
# t-options without consumer, errors
# --------------------------------------------------------------------------
case('options_unconsumed', '<t t-name="main"><span t-options="{}"/></t>')
case('field_on_t', '<t t-name="main"><t t-field="a.b"/></t>')
case('field_no_dot', '<t t-name="main"><span t-field="a"/></t>')
case('field_on_td', '<t t-name="main"><td t-field="a.b"/></t>')
case('dunder_expr', '<t t-name="main"><t t-out="a.__class__"/></t>')
case('syntax_error_expr', '<t t-name="main"><t t-out="a +"/></t>')
case('strict_name_error', '<t t-name="main"><t t-out="missing.attr"/></t>')
case('xml_error', '<t t-name="main"><div></t>')

# --------------------------------------------------------------------------
# namespaces
# --------------------------------------------------------------------------
case('ns_static', '<t t-name="main"><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><use xlink:href="#a"/></svg></t>')
case('ns_prefixed', '<t t-name="main"><foo:bar xmlns:foo="http://foo"><foo:baz foo:x="1">t</foo:baz></foo:bar></t>')
case('ns_dynamic', '<t t-name="main"><svg xmlns="http://www.w3.org/2000/svg"><circle t-att-r="r"/><t t-out="r"/></svg></t>', {'r': 5})
case('ns_dynamic_prefixed', '<t t-name="main"><foo:bar xmlns:foo="http://foo" t-att-a="1"><foo:baz t-attf-foo:y="{{2}}" foo:x="1"/></foo:bar></t>')
case('ns_call', {
    'main': '<t t-name="main"><svg xmlns="http://www.w3.org/2000/svg"><t t-call="sub"/></svg></t>',
    'sub': '<t t-name="sub"><g xmlns="http://www.w3.org/2000/svg"><rect/></g></t>',
})

# --------------------------------------------------------------------------
# expressions
# --------------------------------------------------------------------------
case('expr_arith', '<t t-name="main"><t t-out="a * 2 + b - 1"/>|<t t-out="-a"/>|<t t-out="~a"/>|<t t-out="a &lt;&lt; 2"/>|<t t-out="a &amp; 3"/></t>', {'a': 5, 'b': 3})
case('expr_str_ops', '<t t-name="main"><t t-out="s + \'!\'"/>|<t t-out="s * 2"/>|<t t-out="s[1]"/>|<t t-out="s[1:3]"/>|<t t-out="s[::-1]"/>|<t t-out="len(s)"/></t>', {'s': 'hello'})
case('expr_str_methods', '<t t-name="main"><t t-out="s.upper()"/>|<t t-out="\', \'.join(l)"/>|<t t-out="s.split(\'l\')"/>|<t t-out="s.startswith(\'he\')"/>|<t t-out="\'%s-%d\' % (s, 3)"/></t>', {'s': 'hello', 'l': ['a', 'b']})
case('expr_dict', '<t t-name="main"><t t-out="d[\'a\']"/>|<t t-out="d.get(\'b\', 9)"/>|<t t-out="d.get(\'a\')"/>|<t t-out="list(d.keys())"/>|<t t-out="{\'x\': [1, 2]}[\'x\'][1]"/></t>', {'d': {'a': 1}})
case('expr_object', '<t t-name="main"><t t-out="o.name"/>|<t t-out="o.child.name"/></t>', {'o': O(name='parent', child=O(name='kid'))})
case('expr_builtins', '<t t-name="main"><t t-out="max(1, 5, 3)"/>|<t t-out="min([4, 2])"/>|<t t-out="sum([1, 2, 3])"/>|<t t-out="sorted([3, 1, 2])"/>|<t t-out="abs(-2)"/>|<t t-out="round(2.675, 2)"/>|<t t-out="round(2.5)"/>|<t t-out="int(\'42\')"/>|<t t-out="float(3)"/>|<t t-out="str(1.0)"/>|<t t-out="bool(0)"/>|<t t-out="list(range(3))"/>|<t t-out="len({1: 2})"/></t>')
case('expr_builtin_shadow', '<t t-name="main"><t t-out="len(\'ab\')"/></t>', {'len': 'shadowed'})
case('expr_multiline', '<t t-name="main"><t t-out="a and\n    b"/></t>', {'a': 1, 'b': 2})
case('expr_float_repr', '<t t-name="main"><t t-foreach="fs" t-as="f"><t t-out="f"/>,</t></t>',
     {'fs': [0.1, 1.0, 100.0, 1e16, 1.5e16, 1e-5, 0.0001, 123456789.123, 1/3, -2.5, {'$float': 1e22}, 12345678901234567890.0]})
case('expr_comprehension', '<t t-name="main"><t t-out="[x * 2 for x in items]"/></t>', {'items': [1, 2]}, native=False)
case('expr_lambda', '<t t-name="main"><t t-out="sorted(items, key=lambda x: -x)"/></t>', {'items': [1, 3, 2]}, native=False)
case('expr_fstring', '<t t-name="main"><t t-out="f\'{a}-{b!r}\'"/></t>', {'a': 1, 'b': 'x'}, native=False)
case('expr_kwargs', '<t t-name="main"><t t-out="sorted(items, reverse=True)"/></t>', {'items': [1, 3, 2]}, native=False)
case('expr_tuple_top', '<t t-name="main"><t t-out="1, 2"/></t>')
case('expr_true_false_names', '<t t-name="main"><t t-out="true"/><t t-out="false"/></t>')
case('expr_none_out', '<t t-name="main"><b t-out="None">d</b></t>')
case('expr_bytes', '<t t-name="main"><t t-out="b\'ab\'"/><span t-att-x="b\'cd\'"/><t t-set="v" t-valuef="{{b\'ef\'}}"/><t t-out="v"/></t>')
case('expr_chained_cmp_values', '<t t-name="main"><t t-out="0 &lt; a &lt; 10"/>|<t t-out="0 &lt; a &gt; 10"/></t>', {'a': 5})

# --------------------------------------------------------------------------
# whitespace stress
# --------------------------------------------------------------------------
case('ws_technical_all', '''<t t-name="main">
    <div>
        <t t-set="a" t-value="1"/>
        <t t-set="b">
            text
        </t>
        <t t-if="a">
            <span>if</span>
        </t>
        <t t-else="">
            <span>else</span>
        </t>
        <t t-foreach="[1, 2]" t-as="x">
            <i t-out="x"/>
        </t>
        <t t-out="b"/>
        <p t-if="a">
            p
        </p>
    </div>
</t>''')
case('ws_mixed_tabs', '<t t-name="main">\n\t<a>1</a>\n\t<t t-if="1">\n\t\t<b>2</b>\n\t</t>\n\t<c>3</c>\n</t>')
case('ws_trailing_newlines', '<t t-name="main"><b>x</b>\n\n</t>')
case('ws_if_after_newlines', '<t t-name="main">x\n\n<t t-if="1">y</t>\n\n<t t-if="0">z</t>\n\nw</t>')
case('ws_set_before', '<t t-name="main">a\n    <t t-set="x" t-value="1"/>\n    b</t>')
case('ws_call_before', {'main': '<t t-name="main">a\n    <t t-call="s"/>\n    b</t>', 's': '<t t-name="s">S</t>'})
case('ws_foreach_element', '<t t-name="main">a\n    <i t-foreach="[1]" t-as="x">v</i>\n    b</t>')
case('ws_if_element', '<t t-name="main">a\n    <i t-if="0">v</i>\n    b\n    <i t-if="1">w</i>\n    c</t>')
case('ws_root_element', '<div t-name="main">\n\n  <p>x</p>\n\n</div>')
case('ws_out_slot', {'main': '<t t-name="main"><t t-call="s">\n  body\n</t></t>', 's': '<t t-name="s">[<t t-out="0"/>]</t>'})
