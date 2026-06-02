// DuckDB-WASM shell for the docs site. Only boots on the page that contains
// the `#dp-shell` container, and is safe to re-run under Material's instant
// navigation (it no-ops if already embedded on the current container).
//
// Version notes: only the DuckDB v1.5.3 engine ships quack's wasm binary,
// published under the "next" dist-tag. If the badge does not report v1.5.3,
// change VERSION below.
const VERSION = "next";
const RUN_QUACK = true;

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
    const duckdb = await import(
      `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@${VERSION}/+esm`
    );
    const shell = await import(
      `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/+esm`
    );
    const SHELL_MODULE = `https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm-shell@${VERSION}/dist/shell_bg.wasm`;

    // getJsDelivrBundles() pins the wasm URLs to THIS library version, so the
    // engine matches VERSION. Without COOP/COEP, selectBundle picks eh/mvp.
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
