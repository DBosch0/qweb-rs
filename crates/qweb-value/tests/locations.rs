//! Error locations (`XPath` and start tag of the element). Most are built from
//! the template source only when the error is reported; elements modified
//! before their compilation (root, `t-else`/`t-elif`) are built eagerly.
//! Expected values are the ones of the eager implementation.

#![allow(clippy::expect_used, reason = "test helpers")]

use qweb_value::{Value, ValueHost};

fn location(main: &str) -> (String, String) {
    let host = ValueHost::new();
    host.add_template("main", main);
    host.add_template("sub", r#"<t t-name="sub"><t t-out="0"/></t>"#);
    let x = qweb_value::from_json(&serde_json::json!({}));
    let err = host
        .render("main", [("x", x), ("n", Value::Int(1))])
        .expect_err("the template raises");
    (
        err.path().expect("path").to_owned(),
        err.element().expect("element").to_owned(),
    )
}

#[track_caller]
fn check(main: &str, path: &str, element: &str) {
    assert_eq!(location(main), (path.to_owned(), element.to_owned()));
}

#[test]
fn runtime_error() {
    check(
        r#"<t t-name="main"><div class="a"><p/><span t-out="x['y']" class="b"/><span/></div></t>"#,
        "/t/div/span[1]",
        r#"<span t-out="x['y']" class="b"/>"#,
    );
}

#[test]
fn root_without_t_name() {
    check(
        r#"<div t-name="main" class="r" t-att-title="x['y']"/>"#,
        "/div",
        r#"<div class="r" t-att-title="x['y']"/>"#,
    );
}

#[test]
fn else_branches() {
    check(
        r#"<t t-name="main"><div><p t-if="False"/><p t-else="" t-att-k="x['y']"/></div></t>"#,
        "/t/div/p[2]",
        r#"<p t-else="" t-att-k="x['y']" t-else-valid="True"/>"#,
    );
    check(
        r#"<t t-name="main"><div><p t-if="False"/><!--c--><p t-elif="x['y']" class="z"/></div></t>"#,
        "/t/div/p[2]",
        r#"<p t-elif="x['y']" class="z" t-else-valid="True"/>"#,
    );
}

#[test]
fn lazy_bodies() {
    check(
        r#"<t t-name="main"><t t-set="v"><b t-out="x['y']" id="i"/></t><i t-out="v"/></t>"#,
        "/t/t/b",
        r#"<b t-out="x['y']" id="i"/>"#,
    );
    check(
        r#"<t t-name="main"><section><t t-call="sub"><u t-att-a="x['y']"/></t></section></t>"#,
        "/t/section/t/u",
        r#"<u t-att-a="x['y']"/>"#,
    );
}

#[test]
fn attributes_before_compilation() {
    check(
        r#"<t t-name="main"><ul><li t-foreach="[1,2]" t-as="i" t-att-c="x[i]"/></ul></t>"#,
        "/t/ul/li",
        r#"<li t-foreach="[1,2]" t-as="i" t-att-c="x[i]"/>"#,
    );
    check(
        r#"<t t-name="main"><div title.translate="t" t-att-v="x['y']"/></t>"#,
        "/t/div",
        r#"<div title.translate="t" t-att-v="x['y']"/>"#,
    );
    check(
        r#"<t t-name="main"><t t-call="sub" a="x['y']" b.f="z"/></t>"#,
        "/t/t",
        r#"<t t-call="sub" a="x['y']" b.f="z"/>"#,
    );
}

#[test]
fn compile_error() {
    check(
        r#"<t t-name="main"><div><p/><p t-att-x="1 +"/></div></t>"#,
        "/t/div/p[2]",
        r#"<p t-att-x="1 +"/>"#,
    );
}
