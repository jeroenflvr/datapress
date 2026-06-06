// Bundles the local DuckDB-WASM ESM glue (docs/external/*.mjs) into
// self-contained browser ESM served from docs/src/assets/vendor/duckdb/.
//
// Why: docs/external/duckdb-browser.mjs and shell.mjs are the raw npm `dist`
// builds. They import bare specifiers (apache-arrow, xterm, xterm-addon-*)
// which a browser cannot resolve. esbuild inlines those deps so the shell runs
// fully self-hosted with NO CDN at runtime.
//
// CRITICAL: apache-arrow MUST be 17.0.0. The engine glue uses arrow >=7 APIs
// (makeData / tableToIPC / `new arrow.Table(reader)`). arrow 6 throws
// "Table must be initialized with a Schema or at least one RecordBatch" and the
// version()/quack queries fail silently. (duckdb-wasm's package.json `^6.0.1`
// is misleading — the real build used arrow 17.)
//
// Run via: task docs:vendor-duckdb
//
// Requires Node + npx with esbuild and the deps below available. The task
// installs them into a throwaway dir and passes its node_modules path as
// DUCKDB_BUNDLE_NM.
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const DOCS = resolve(here, "..");
const EXT = resolve(DOCS, "external");
const OUT = resolve(DOCS, "src/assets/vendor/duckdb");

const NM = process.env.DUCKDB_BUNDLE_NM;
if (!NM) {
  console.error("DUCKDB_BUNDLE_NM (path to node_modules) is required");
  process.exit(1);
}

// esbuild lives in the throwaway build node_modules, not next to this script,
// so import it by absolute path rather than bare specifier.
const esbuild = await import(
  pathToFileURL(resolve(NM, "esbuild/lib/main.js")).href
);

const aliases = {
  // Browser ESM entry (Arrow.dom.mjs), pinned to apache-arrow@17.0.0.
  "apache-arrow": resolve(NM, "apache-arrow/Arrow.dom.mjs"),
  xterm: resolve(NM, "xterm/lib/xterm.js"),
  "xterm-addon-fit": resolve(NM, "xterm-addon-fit/lib/xterm-addon-fit.js"),
  "xterm-addon-web-links": resolve(
    NM,
    "xterm-addon-web-links/lib/xterm-addon-web-links.js"
  ),
  "xterm-addon-webgl": resolve(NM, "xterm-addon-webgl/lib/xterm-addon-webgl.js"),
};

const common = {
  bundle: true,
  format: "esm",
  platform: "browser",
  legalComments: "none",
};

await esbuild.build({
  ...common,
  entryPoints: [resolve(EXT, "duckdb-browser.mjs")],
  alias: aliases,
  outfile: resolve(OUT, "duckdb-browser.bundled.mjs"),
});
console.log("engine bundled -> duckdb-browser.bundled.mjs");

await esbuild.build({
  ...common,
  entryPoints: [resolve(EXT, "shell.mjs")],
  // The shell imports @duckdb/duckdb-wasm; point it at the local engine source.
  alias: { ...aliases, "@duckdb/duckdb-wasm": resolve(EXT, "duckdb-browser.mjs") },
  outfile: resolve(OUT, "shell.bundled.mjs"),
});
console.log("shell bundled -> shell.bundled.mjs");
