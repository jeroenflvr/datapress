// DataPress full-page DuckDB-WASM shell. Served at
// {explorer_base}/assets/terminal.js and embedded via include_str!.
//
// Fully self-hosted: the engine/shell WebAssembly, worker scripts and the
// bundled ESM glue are served from the binary itself under
// {explorer_base}/assets/vendor/duckdb/ (embedded via include_dir!, refreshed
// by `task docs:vendor-duckdb`). No CDN is contacted at runtime. The shell
// still installs the `quack` extension from the DuckDB extension repository so
// you can attach this (or any) DataPress server and query it remotely:
//   CREATE SECRET (TYPE quack, TOKEN '…'); ATTACH 'quack:host' AS r;
//   SELECT * FROM r.<dataset> LIMIT 10;
// For a remote host over plain HTTP (no TLS), add DISABLE_SSL true, e.g.
//   ATTACH 'quack:host' AS r (TOKEN '…', DISABLE_SSL true);
//
// Version notes: the DuckDB v1.5.3 engine ships quack's wasm binary; it is
// bundled by @duckdb/duckdb-wasm@1.11.0. VERSION is only used for the boot
// label now — the actual code is loaded from the vendored assets below.
const VERSION = "1.11.0";
const RUN_QUACK = true;

// Vendored DuckDB-WASM assets, resolved relative to this module's URL so they
// keep working under any explorer mount point (e.g. /explore/assets/...).
const VENDOR_BASE = new URL("vendor/duckdb/", import.meta.url);
const vendor = (file) => new URL(file, VENDOR_BASE).href;

const boot = document.getElementById("boot");
const toast = document.getElementById("toast");
const showToast = (msg, kind) => {
  toast.textContent = msg;
  toast.className = "show " + (kind || "");
  if (kind === "ok") setTimeout(() => toast.classList.remove("show"), 12000);
};

(async () => {
  try {
    if (location.protocol === "file:") {
      boot.innerHTML = "Serve over HTTP, don't open from disk (file://).";
      return;
    }

    boot.textContent = "loading engine \u2014 @duckdb/duckdb-wasm@" + VERSION + " \u2026";
    const duckdb = await import(vendor("duckdb-browser.bundled.mjs"));
    const shell = await import(vendor("shell.bundled.mjs"));
    const SHELL_MODULE = vendor("shell_bg.wasm");

    // Self-hosted bundle: point the engine wasm + worker at the vendored
    // copies instead of duckdb.getJsDelivrBundles(). selectBundle picks eh/mvp
    // based on platform features (coi needs COOP/COEP, absent here, so it falls
    // back to eh and then mvp).
    const bundle = await duckdb.selectBundle({
      mvp: {
        mainModule: vendor("duckdb-mvp.wasm"),
        mainWorker: vendor("duckdb-browser-mvp.worker.js"),
      },
      eh: {
        mainModule: vendor("duckdb-eh.wasm"),
        mainWorker: vendor("duckdb-browser-eh.worker.js"),
      },
    });
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker}");`], { type: "text/javascript" })
    );
    const worker = new Worker(workerUrl);
    const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
    const db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    URL.revokeObjectURL(workerUrl);
    await db.open({ path: ":memory:", allowUnsignedExtensions: true });

    //  version check + quack extension 
    let note = null;
    {
      const conn = await db.connect();
      let ver = "?";
      try { ver = (await conn.query("SELECT version() AS v;")).toArray()[0].v; } catch (_) {}
      if (RUN_QUACK) {
        boot.textContent = "loading quack \u2026";
        try {
          await conn.query("INSTALL quack FROM core_nightly;");
          await conn.query("LOAD quack;");
          note = [`DuckDB ${ver} \u2014 quack loaded. Connect with: CREATE SECRET (TYPE quack, TOKEN '\u2026'); ATTACH 'quack:host' AS r; (remote host over plain HTTP? add DISABLE_SSL true)`, "ok"];
        } catch (e) {
          note = [`DuckDB ${ver} \u2014 quack not loaded: ${e.message || e}`, "err"];
        }
      } else {
        note = [`DuckDB ${ver}`, "ok"];
      }
      await conn.close();
    }

    await shell.embed({
      shellModule: SHELL_MODULE,
      container: document.getElementById("shell"),
      resolveDatabase: async () => db,
    });
    boot.classList.add("hide");
    if (note) showToast(note[0], note[1]);
  } catch (err) {
    console.error(err);
    boot.textContent = "failed to load: " + (err && err.message ? err.message : err);
  }
})();
