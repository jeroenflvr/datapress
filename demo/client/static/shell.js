// DuckDB-WASM shell bootstrap. Ported from the original inline <script>, now
// driven by config injected from the Jinja template (window.__DUCKDB_CONFIG__).
//
// Version notes: the npm "latest"/dev builds are currently on DuckDB v1.5.1,
// where quack's wasm binary is NOT published — only v1.5.3 has it. The "next"
// dist-tag is the likeliest 1.5.3 build. If the engine badge does NOT report
// v1.5.3, set DUCKDB_WASM_VERSION on the server to try another tag, e.g.
// "1.33.1-dev45.0" or "latest" (currently v1.5.1 — quack will 404).

const { version: VERSION, runQuack: RUN_QUACK } = window.__DUCKDB_CONFIG__;

const boot = document.getElementById("boot");
const badge = document.getElementById("engine-badge");
const toastContainer = document.getElementById("toast");

function showToast(msg, kind) {
  const cls = kind === "err" ? "text-bg-danger" : "text-bg-success";
  const el = document.createElement("div");
  el.className = `toast align-items-center ${cls} border-0 show`;
  el.setAttribute("role", "alert");
  el.innerHTML = `
    <div class="d-flex">
      <div class="toast-body font-monospace small">${msg}</div>
      <button type="button" class="btn-close btn-close-white me-2 m-auto" aria-label="Close"></button>
    </div>`;
  el.querySelector(".btn-close").addEventListener("click", () => el.remove());
  toastContainer.appendChild(el);
  if (kind !== "err") setTimeout(() => el.remove(), 8000);
}

(async () => {
  try {
    if (location.protocol === "file:") {
      boot.innerHTML =
        "Serve over HTTP, don't open from disk (file://).<br><br>" +
        "Run: <code>uv run uvicorn main:app --reload</code>";
      return;
    }

    boot.textContent = `loading engine — @duckdb/duckdb-wasm@${VERSION} …`;
    const duckdb = await import(
      `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@${VERSION}/+esm`
    );
    const shell = await import(
      `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/+esm`
    );
    const SHELL_MODULE = `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/dist/shell_bg.wasm`;

    // getJsDelivrBundles() returns wasm URLs pinned to THIS library version, so
    // the engine that boots matches VERSION. Without COOP/COEP, selectBundle
    // picks the eh/mvp build (no cross-origin isolation needed).
    const bundle = await duckdb.selectBundle(duckdb.getJsDelivrBundles());
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker}");`], {
        type: "text/javascript",
      })
    );
    const worker = new Worker(workerUrl);
    const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
    const db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    URL.revokeObjectURL(workerUrl);

    await db.open({ path: ":memory:", allowUnsignedExtensions: true });

    // ── version check + quack ─────────────────────────────────────────────
    let note = null;
    {
      const conn = await db.connect();
      let ver = "?";
      try {
        ver = (await conn.query("SELECT version() AS v;")).toArray()[0].v;
      } catch (_) {}
      badge.textContent = `DuckDB ${ver}`;
      badge.className = "badge text-bg-success ms-auto";
      if (RUN_QUACK) {
        boot.textContent = "loading quack …";
        try {
          await conn.query("INSTALL quack FROM core_nightly;");
          await conn.query("LOAD quack;");
          note = [
            `DuckDB ${ver} — quack loaded. Connect with: CREATE SECRET (TYPE quack, TOKEN '…'); ATTACH 'quack:host' AS r;`,
            "ok",
          ];
        } catch (e) {
          note = [`DuckDB ${ver} — quack not loaded: ${e.message || e}`, "err"];
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
    badge.textContent = "engine failed";
    badge.className = "badge text-bg-danger ms-auto";
    boot.textContent = "failed to load: " + (err && err.message ? err.message : err);
  }
})();
