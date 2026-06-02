// DataPress full-page DuckDB-WASM shell. Served at
// {explorer_base}/assets/terminal.js and embedded via include_str!.
//
// Version notes: only the DuckDB v1.5.3 engine ships quack's wasm binary,
// published under the "next" dist-tag. If the badge does not report v1.5.3,
// change VERSION below. The shell loads quack so you can attach this (or any)
// DataPress server and query its datasets remotely:
//   CREATE SECRET (TYPE quack, TOKEN '…'); ATTACH 'quack:host' AS r;
//   SELECT * FROM r.<dataset> LIMIT 10;
const VERSION = "next";
const RUN_QUACK = true;

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
    const duckdb = await import(`https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@${VERSION}/+esm`);
    const shell = await import(`https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/+esm`);
    const SHELL_MODULE = `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/dist/shell_bg.wasm`;

    // getJsDelivrBundles() pins the wasm URLs to THIS library version, so the
    // engine matches VERSION. Without COOP/COEP, selectBundle picks eh/mvp.
    const bundle = await duckdb.selectBundle(duckdb.getJsDelivrBundles());
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker}");`], { type: "text/javascript" })
    );
    const worker = new Worker(workerUrl);
    const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
    const db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    URL.revokeObjectURL(workerUrl);
    await db.open({ path: ":memory:", allowUnsignedExtensions: true });

    // ── version check + quack extension ──────────────────────────────
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
          note = [`DuckDB ${ver} \u2014 quack loaded. Connect with: CREATE SECRET (TYPE quack, TOKEN '\u2026'); ATTACH 'quack:host' AS r;`, "ok"];
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
