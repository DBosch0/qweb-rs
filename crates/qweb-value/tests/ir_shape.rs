//! Shape of the compiled statements for the two cases the compiler
//! specialises: the loop variables a `t-foreach` assigns at every iteration
//! and the attributes rendered without a dict. The output is the same either
//! way (see the conformance cases); these tests pin when each form is used.

use std::sync::Arc;

use qweb_core::Engine;
use qweb_core::host::TemplateRef;
use qweb_core::ir::{LoopVars, Program, Stmt};
use qweb_value::{Context, ValueHost};

/// The compiled template `main`, given its content.
fn compile(content: &str) -> qweb_core::Result<Arc<Program<ValueHost>>> {
    let host = ValueHost::new();
    host.add_template("main", &format!(r#"<t t-name="main">{content}</t>"#));
    host.add_template("sub", r#"<t t-name="sub"/>"#);
    Engine::new(host).program(&TemplateRef::parse("main"), &Context::default())
}

/// `each` of the first `t-foreach` of the template.
fn each(content: &str) -> Option<LoopVars> {
    let program = compile(content).ok()?;
    program.blocks[program.main].iter().find_map(|s| match s {
        Stmt::Foreach(f) => Some(f.each),
        _ => None,
    })
}

/// The rendered parts of the first attributes rendered without a dict.
fn direct(content: &str) -> Option<Vec<String>> {
    let program = compile(content).ok()?;
    program.blocks[program.main].iter().find_map(|s| match s {
        Stmt::Attrs { attrs, .. } => Some(attrs.iter().map(|a| a.rendered.to_string()).collect()),
        _ => None,
    })
}

#[test]
fn loop_vars_assigned_at_each_iteration() {
    let lp = |body: &str| each(&format!(r#"<t t-foreach="xs" t-as="x">{body}</t>"#));
    // only what the body reads; `x` and `x_size` are not in the set
    assert_eq!(lp(r#"<i t-out="x"/>"#), Some(LoopVars::NONE));
    assert_eq!(lp(r#"<i t-out="x_size"/>"#), Some(LoopVars::NONE));
    assert_eq!(lp(r#"<i t-out="x_index + 1"/>"#), Some(LoopVars::INDEX));
    assert_eq!(
        lp(r#"<i t-att-class="x_parity" t-if="not x_last">,</i>"#),
        Some(LoopVars::PARITY.union(LoopVars::LAST))
    );
    assert_eq!(
        lp(r#"<t t-foreach="x" t-as="y"><i t-attf-id="{{ x_odd }}-{{ y_first }}"/></t>"#),
        Some(LoopVars::ODD)
    );
    assert_eq!(
        lp(r#"<t t-set="v" t-value="x_value"/>"#),
        Some(LoopVars::VALUE)
    );
    // other names with the same prefix are not loop variables
    assert_eq!(
        lp(r#"<i t-out="x_indexes"/><i t-out="xs"/>"#),
        Some(LoopVars::NONE)
    );
    // the body gives the scope away, or assigns a loop variable
    for body in [
        r#"<t t-call="sub"/>"#,
        r#"<t t-set="c">lazy</t>"#,
        r#"<t t-set="{'x_index': 1}"/>"#,
        r#"<t t-set="x_first" t-value="1"/>"#,
        r#"<t t-set="x" t-value="1"/>"#,
        r#"<t t-foreach="xs" t-as="x_odd"/>"#,
        r#"<t t-foreach="xs" t-as="x"/>"#,
        r#"<i t-out="x" t-options="{'widget': 'text'}"/>"#,
        r#"<i t-field="x.name"/>"#,
    ] {
        assert_eq!(lp(body), Some(LoopVars::ALL), "{body}");
    }
    // a loop whose variables only share a prefix assigns none of them
    assert_eq!(
        lp(r#"<t t-foreach="x" t-as="x_line"><i t-out="x_line"/></t>"#),
        Some(LoopVars::NONE)
    );
}

#[test]
fn attributes_without_dict() {
    // set and rendered in a row: one statement (static attributes, then
    // dynamic ones in source order), constant values rendered when compiling
    assert_eq!(
        direct(r#"<a class="c &amp; d" t-att-href="u" t-attf-id="i{{ n }}" t-attf-x="k">x</a>"#),
        Some(vec![
            r#" class="c &amp; d""#.to_owned(),
            r#" href=""#.to_owned(),
            r#" id=""#.to_owned(),
            r#" x="k""#.to_owned(),
        ])
    );
    assert_eq!(
        direct(r#"<a href="javascript:alert(1)" t-att-id="n"/>"#),
        Some(vec![r#" href="""#.to_owned(), r#" id=""#.to_owned()])
    );
    // the dict is needed: same name twice, `t-att` mapping, or attributes
    // rendered only if the element has content
    for content in [
        r#"<a class="c" t-att-class="u"/>"#,
        r#"<a t-att="{'a': 1}" t-att-id="n"/>"#,
        r#"<a t-att-href="u" t-out="n"/>"#,
    ] {
        assert_eq!(direct(content), None, "{content}");
    }
}
