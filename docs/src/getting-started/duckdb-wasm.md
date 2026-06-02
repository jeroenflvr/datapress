# DuckDB-WASM terminal

A full DuckDB shell, running entirely in your browser via
[DuckDB-WASM](https://github.com/duckdb/duckdb-wasm) — no install, no server.
It boots an in-memory database and loads the `quack` extension, so you can
connect straight to a DataPress server from the prompt.

!!! tip "Connect to a DataPress server"
    Once the engine reports `quack loaded`, attach a running server:

    ```sql
    CREATE SECRET (TYPE quack, TOKEN '…');
    ATTACH 'quack:your-server-host' AS r;
    SELECT * FROM r.accidents LIMIT 10;
    ```

<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/xterm/css/xterm.css" />

<div id="dp-shell-card" class="dp-shell-card" markdown>

<div class="dp-shell-actions">
<a href="../../assets/duckdb-terminal.html" target="_blank" rel="noopener" class="dp-shell-btn" title="Open the terminal in a new tab">↗ Open in new tab</a>
<button id="dp-shell-fullscreen" class="dp-shell-btn" type="button" title="Toggle fullscreen" aria-label="Toggle fullscreen">⛶ Fullscreen</button>
</div>
<div id="dp-shell-boot" class="dp-shell-boot">loading duckdb&hellip;</div>
<div id="dp-shell"></div>

</div>

<div id="dp-shell-toast" class="dp-shell-toast"></div>

The engine and `quack` extension are fetched from jsDelivr on first use, so the
initial load takes a few seconds. Everything after that runs locally in the
WebAssembly sandbox.

## Notes

- **In-memory only.** The database lives in the tab; reloading the page starts
  fresh.
- **Network access** is needed to download the WASM bundles and to reach any
  `quack:` server you attach.
- **Cross-origin isolation** is not required — the shell picks the `eh`/`mvp`
  build automatically when `COOP`/`COEP` headers are absent.
