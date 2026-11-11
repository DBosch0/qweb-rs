//! Runs the conformance cases (expected output from the reference Python
//! engine, see `tests/conformance/gen_expected.py`) through the pure-Rust host.

use qweb_core::{Engine, ErrorKind};
use qweb_value::{Context, ValueHost, from_json};
use serde_json::Value as J;

fn error_name(kind: &ErrorKind) -> String {
    match kind {
        ErrorKind::Syntax(_) => "SyntaxError".into(),
        ErrorKind::Value(_) => "ValueError".into(),
        ErrorKind::Key(_) => "KeyError".into(),
        ErrorKind::Type(_) => "TypeError".into(),
        ErrorKind::Assertion(_) => "AssertionError".into(),
        ErrorKind::NotImplemented(_) => "NotImplementedError".into(),
        ErrorKind::Recursion(_) => "RecursionError".into(),
        ErrorKind::Xml(_) => "XMLSyntaxError".into(),
        ErrorKind::Other(m) => m.split(':').next().unwrap_or("Error").to_owned(),
        ErrorKind::Host(_) => "HostError".into(),
    }
}

#[test]
fn conformance() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/conformance/cases.json"
    );
    let cases: Vec<J> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let filter = std::env::var("CASE").ok();
    let mut failures = Vec::new();
    let mut ran = 0;
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        if !case["native"].as_bool().unwrap()
            || filter.as_deref().is_some_and(|f| !name.contains(f))
        {
            continue;
        }
        ran += 1;
        let host = ValueHost::new();
        for (tname, src) in case["templates"].as_object().unwrap() {
            host.add_template(tname, src.as_str().unwrap());
        }
        let values: Vec<(String, qweb_value::Value)> = case["values"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), from_json(v)))
            .collect();
        let scope = host.make_scope(values.iter().map(|(k, v)| (k.as_str(), v.clone())));
        let ctx = Context::new(
            case["options"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), from_json(v))),
        );
        let engine = Engine::new(host.clone());
        let result = engine.render(
            &qweb_core::TemplateRef::parse(case["main"].as_str().unwrap()),
            scope,
            ctx,
        );
        match (result, case.get("output").and_then(J::as_str)) {
            (Ok(out), Some(expected)) => {
                if out != expected {
                    failures.push(format!(
                        "{name}: output mismatch\n  expected: {expected:?}\n  got:      {out:?}"
                    ));
                }
            }
            (Ok(out), None) => failures.push(format!(
                "{name}: expected error {} ({}), got output {out:?}",
                case["cause"], case["message"]
            )),
            (Err(e), Some(expected)) => failures.push(format!(
                "{name}: unexpected error {e}\n  expected: {expected:?}"
            )),
            (Err(e), None) => {
                let got = error_name(e.kind());
                let want = case["cause"].as_str().unwrap_or("");
                if got != want {
                    failures.push(format!(
                        "{name}: expected error {want}, got {got}: {}",
                        e.message()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {ran} cases failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    eprintln!("{ran} conformance cases passed");
}
