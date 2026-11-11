# qweb-wasm

[Odoo](https://github.com/odoo/odoo)'s QWeb template inheritance and string
rendering, compiled from Rust to WebAssembly. It is part of
[qweb-rs](https://github.com/DBosch0/qweb-rs), a port of Odoo 20's QWeb engine
whose goal is byte-identical output to the original.

- `applyInheritanceServer`: template inheritance, server dialect
  (`odoo/tools/template_inheritance.py`), on XML strings.
- `TemplateStore`: template inheritance, browser dialect
  (`web/static/src/core/template_inheritance.js`). Trees stay in WASM memory
  and are referenced by handles.
- `Renderer`: QWeb string rendering (server semantics) with JSON values.

> **Status:** experimental.

## Install

```sh
npm install qweb-wasm
```

## Browsers and bundlers (Vite, webpack 5, ...)

Initialize the module once before using it:

```js
import init, { Renderer, applyInheritanceServer } from "qweb-wasm";

await init();

const r = new Renderer();
r.addTemplate("hello", '<t t-name="hello"><p t-out="name"/></t>');
r.render("hello", JSON.stringify({ name: "<world>" }), ""); // "<p>&lt;world&gt;</p>"
r.free();
```

`init()` fetches `qweb_wasm_bg.wasm` next to the JS file. To load it from
somewhere else, pass its URL or bytes: `await init({ module_or_path: url })`.

## Node.js (18+)

Node gets a build that loads synchronously, so there is no `init()`. Both
`import` and `require` work:

```js
import { applyInheritanceServer } from "qweb-wasm";

applyInheritanceServer(
  '<t t-name="x"><p>hi</p></t>',
  ['<xpath expr="//p" position="after"><span>new</span></xpath>'],
  false, // inherit_branding
); // '<t t-name="x"><p>hi</p><span>new</span></t>'
```

## Memory

`Renderer` and `TemplateStore` live in WASM memory: call `.free()` when you
are done with them.

## License

LGPL-3.0-only, like the Odoo code it is ported from.

Odoo is a trademark of Odoo S.A. This project is not affiliated with or
endorsed by Odoo S.A.
