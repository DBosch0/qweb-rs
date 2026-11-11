//! `Debug` of the engine types, which exists when the host types are `Debug`.

use std::sync::Arc;

use qweb_core::Engine;
use qweb_core::host::TemplateRef;
use qweb_value::{Context, ValueHost};

#[test]
fn program_and_engine_debug() {
    let host = ValueHost::new();
    host.add_template("main", r#"<t t-name="main"><p t-out="x"/></t>"#);
    let engine: Arc<Engine<ValueHost>> = Engine::new(host);
    let program = engine
        .program(&TemplateRef::parse("main"), &Context::default())
        .unwrap();
    let text = format!("{program:?}");
    assert!(text.contains("Program"), "{text}");
    assert!(format!("{engine:?}").contains("Engine"));
}
