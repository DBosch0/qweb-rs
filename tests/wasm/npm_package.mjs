// Test the packed npm package the way users get it: install the tarball into a
// temporary project, then load it through `import`, `require` and the web build.
// Run: node tests/wasm/npm_package.mjs <qweb-wasm-X.Y.Z.tgz>
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const tarball = path.resolve(process.argv[2]);
const checks = new URL("checks.mjs", import.meta.url).href;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "qweb-wasm-npm-"));
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const run = (file, code) => {
    fs.writeFileSync(path.join(dir, file), code);
    execFileSync(process.execPath, [file], { cwd: dir, stdio: "inherit" });
};

try {
    fs.writeFileSync(path.join(dir, "package.json"), '{ "private": true }\n');
    execFileSync(npm, ["install", "--no-audit", "--no-fund", "--silent", tarball], {
        cwd: dir,
        stdio: "inherit",
        shell: process.platform === "win32",
    });

    // Node, ESM: the "node" export (synchronous, no init).
    run("esm.mjs", `
        import * as api from "qweb-wasm";
        import { runChecks } from ${JSON.stringify(checks)};
        runChecks(api);
        console.log("import (node build): ok");
    `);

    // Node, CommonJS.
    run("cjs.mjs", `
        import { createRequire } from "node:module";
        import { runChecks } from ${JSON.stringify(checks)};
        runChecks(createRequire(import.meta.url)("qweb-wasm"));
        console.log("require (node build): ok");
    `);

    // The web build (what browsers and bundlers get), initialized with the bytes.
    run("web.mjs", `
        import fs from "node:fs";
        import { createRequire } from "node:module";
        import { runChecks } from ${JSON.stringify(checks)};
        const require = createRequire(import.meta.url);
        const dir = require("path").dirname(require.resolve("qweb-wasm/package.json"));
        const web = await import(require("url").pathToFileURL(dir + "/web/qweb_wasm.js").href);
        await web.default({ module_or_path: fs.readFileSync(dir + "/web/qweb_wasm_bg.wasm") });
        runChecks(web);
        console.log("web build: ok");
    `);
} finally {
    fs.rmSync(dir, { recursive: true, force: true });
}
