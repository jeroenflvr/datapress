// DuckDB-WASM shell for the docs site. Only boots on the page that contains
// the `#dp-shell` container, and is safe to re-run under Material's instant
// navigation (it no-ops if already embedded on the current container).
//
// Version notes: only the DuckDB v1.5.3 engine ships quack's wasm binary.
// That engine is bundled by @duckdb/duckdb-wasm@1.11.0. VERSION is only used
// for the boot label now; the actual code is fully self-hosted.
const VERSION = "1.11.0";
const RUN_QUACK = true;

// Fully self-hosted DuckDB-WASM. Everything is served from the docs site
// itself (docs/src/assets/vendor/duckdb) — no CDN at runtime:
//   - duckdb-browser.bundled.mjs / shell.bundled.mjs : the ESM glue, bundled
//     from the local docs/external builds with apache-arrow + xterm inlined
//     (see Taskfile `docs:vendor-duckdb`).
//   - duckdb-*.wasm + *.worker.js : the engine/shell WebAssembly + workers.
// Resolved via import.meta.url so they keep working when the site is served
// under a sub-path (e.g. the embedded `/mkdocs/` route).
const VENDOR_BASE = new URL("../vendor/duckdb/", import.meta.url);
const vendor = (file) => new URL(file, VENDOR_BASE).href;

function showToast(container, msg, kind) {
  if (!container) return;
  const el = document.createElement("div");
  el.className = "dp-shell-toast-item " + (kind || "");
  el.textContent = msg;
  container.appendChild(el);
  if (kind !== "err") setTimeout(() => el.remove(), 8000);
}

// Wire the fullscreen button to expand the terminal card. Idempotent: re-runs
// under instant navigation just re-bind the (possibly new) button element.
function wireFullscreen() {
  const card = document.getElementById("dp-shell-card");
  const btn = document.getElementById("dp-shell-fullscreen");
  if (!card || !btn || btn.dataset.wired === "1") return;
  btn.dataset.wired = "1";

  btn.addEventListener("click", () => {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else if (card.requestFullscreen) {
      card.requestFullscreen().catch((err) => console.error(err));
    }
  });

  document.addEventListener("fullscreenchange", () => {
    const active = document.fullscreenElement === card;
    card.classList.toggle("is-fullscreen", active);
    btn.textContent = active ? "⛶ Exit fullscreen" : "⛶ Fullscreen";
  });
}

async function bootShell() {
  const shellEl = document.getElementById("dp-shell");
  // Nothing to do on pages without the terminal, or if we already booted it.
  if (!shellEl || shellEl.dataset.booted === "1") return;
  shellEl.dataset.booted = "1";

  const boot = document.getElementById("dp-shell-boot");
  const toast = document.getElementById("dp-shell-toast");
  wireFullscreen();

  try {
    boot.textContent = `loading engine — @duckdb/duckdb-wasm@${VERSION} …`;
    const duckdb = await import(vendor("duckdb-browser.bundled.mjs"));
    const shell = await import(vendor("shell.bundled.mjs"));
    // Shell terminal wasm, served locally instead of from jsDelivr.
    const SHELL_MODULE = vendor("shell_bg.wasm");

    // Self-hosted bundle: point the engine wasm + worker at the vendored copies
    // instead of duckdb.getJsDelivrBundles(). selectBundle still picks eh/mvp
    // based on platform features (coi needs COOP/COEP, which the docs site does
    // not set, so it falls back to eh and then mvp).
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
      if (RUN_QUACK) {
        boot.textContent = "loading quack …";
        try {
          await conn.query("INSTALL quack FROM core_nightly;");
          await conn.query("LOAD quack;");
          note = [
            `DuckDB ${ver} — quack loaded. Connect with: CREATE SECRET (TYPE quack, TOKEN '…'); ATTACH 'quack:host' AS r; (remote host over plain HTTP? add disable_ssl true)`,
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
      container: shellEl,
      resolveDatabase: async () => db,
    });
    boot.classList.add("hide");
    if (note) showToast(toast, note[0], note[1]);
  } catch (err) {
    console.error(err);
    shellEl.dataset.booted = "";
    if (boot) {
      boot.textContent =
        "failed to load: " + (err && err.message ? err.message : err);
    }
  }
}

// Material's instant navigation exposes `document$`; fall back to a plain load.
if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(() => {
    wireFullscreen();
    bootShell();
  });
} else {
  wireFullscreen();
  bootShell();
}
