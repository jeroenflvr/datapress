// DataPress explorer — API Query tab. Served at
// {explorer_base}/assets/query-api.js and embedded via include_str!.
//
// Queries the live HTTP API two ways:
//   * Structured JSON  → POST {apiBase}/datasets/<name>/query  (QueryRequest body)
//   * Raw SQL          → POST {apiBase}/sql                    ({ sql, max_rows })
// Results render as a table and export to CSV / JSON / Parquet. CSV and JSON
// are produced in pure JS; Parquet is written by the locally-vendored
// DuckDB-WASM engine (no CDN) loaded lazily on first export.

const config = JSON.parse(document.getElementById("explorer-config").textContent || "{}");
const datasets = JSON.parse(document.getElementById("datasets-data").textContent || "[]");

const explorerBase = config.explorerBase || "";
const apiBase = config.apiBase || "";
const sqlEnabled = config.sqlEnabled === true;

const el = (id) => document.getElementById(id);
const statusEl = el("api-status");
const datasetEl = el("api-dataset");
const jsonModeEl = el("api-json-mode");
const sqlModeEl = el("api-sql-mode");
const jsonBodyEl = el("api-json-body");
const jsonUrlEl = el("api-json-url");
const sqlBodyEl = el("api-sql-body");
const sqlMaxRowsEl = el("api-sql-maxrows");
const sqlDisabledEl = el("api-sql-disabled");
const checkEl = el("api-check");
const errEl = el("api-error");
const timingEl = el("api-timing");
const exportEl = el("api-export");
const resEl = el("api-results");
const headersEl = el("api-headers");
const formatArrowEl = el("api-format-arrow");

let lastRows = [];
let lastCols = [];

// ── small helpers ──────────────────────────────────────────────────────────
const setStatus = (text, cls) => {
  statusEl.textContent = text;
  statusEl.className = `badge ms-auto text-bg-${cls || "secondary"}`;
};

function showCheck(message, kind) {
  checkEl.textContent = message;
  checkEl.className = `alert mt-3 mono alert-${kind === "ok" ? "success" : "warning"}`;
  checkEl.classList.remove("d-none");
}

function clearMessages() {
  checkEl.classList.add("d-none");
  errEl.classList.add("d-none");
}

// ── timing / size readout ────────────────────────────────────────────────────
function formatMs(ms) {
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 1000) return `${ms.toFixed(1)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function formatBytes(n) {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

function showTiming({ ttfb, total, bytes, rows, format }) {
  const item = (icon, label, value) =>
    `<span title="${label}"><i class="bi ${icon}"></i> ${value}</span>`;
  const parts = [
    item("bi-hourglass-split", "Time to first byte", `TTFB ${formatMs(ttfb)}`),
    item("bi-stopwatch", "Total round-trip time", `total ${formatMs(total)}`),
    item("bi-download", "Response body size", formatBytes(bytes)),
  ];
  if (rows != null) parts.push(item("bi-table", "Rows returned", `${rows} row(s)`));
  if (format) parts.push(item("bi-file-earmark-binary", "Response wire format", format));
  timingEl.innerHTML = parts.join("");
  timingEl.classList.remove("d-none");
}

function clearTiming() {
  timingEl.classList.add("d-none");
  timingEl.innerHTML = "";
}

function showError(message) {
  errEl.textContent = message;
  errEl.classList.remove("d-none");
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

function selectedDataset() {
  return datasetEl.value || (datasets[0] && datasets[0].name) || "";
}

// ── request headers (custom + format-derived Accept) ─────────────────────────
const ARROW_ACCEPT = "application/vnd.apache.arrow.stream";

function parseHeaderLines() {
  const out = [];
  for (const line of (headersEl.value || "").split("\n")) {
    const t = line.trim();
    if (!t || t.startsWith("#")) continue;
    const idx = t.indexOf(":");
    if (idx < 1) continue;
    const name = t.slice(0, idx).trim();
    const value = t.slice(idx + 1).trim();
    if (name) out.push([name, value]);
  }
  return out;
}

function buildHeaders(wantArrow) {
  const h = new Headers();
  h.set("Content-Type", "application/json");
  h.set("Accept", wantArrow ? ARROW_ACCEPT : "application/json");
  for (const [k, v] of parseHeaderLines()) {
    // Browsers reject forbidden header names (Host, Content-Length, …) — skip
    // those rather than aborting the whole request.
    try { h.set(k, v); } catch { /* ignore forbidden header */ }
  }
  return h;
}

// Locally-vendored Apache Arrow (UMD), loaded lazily to decode Arrow IPC.
let arrowReady = null;
function ensureArrow() {
  if (arrowReady) return arrowReady;
  arrowReady = new Promise((resolve, reject) => {
    if (window.Arrow) { resolve(window.Arrow); return; }
    const s = document.createElement("script");
    s.src = `${explorerBase}/assets/vendor/arrow/arrow.es2015.min.js`;
    s.onload = () => (window.Arrow ? resolve(window.Arrow) : reject(new Error("Arrow bundle loaded but global missing")));
    s.onerror = () => reject(new Error("Failed to load the Arrow bundle"));
    document.head.appendChild(s);
  });
  return arrowReady;
}

function sampleJsonBody() {
  return JSON.stringify({ page: 1, page_size: 100 }, null, 2);
}

function updateJsonUrl() {
  const name = selectedDataset();
  jsonUrlEl.textContent = `${apiBase}/datasets/${name}/query`;
}

// ── populate dataset select + defaults ──────────────────────────────────────
for (const d of datasets) {
  const opt = document.createElement("option");
  opt.value = d.name;
  opt.textContent = `${d.name} (${d.rows} rows)`;
  datasetEl.appendChild(opt);
}
jsonBodyEl.value = sampleJsonBody();
updateJsonUrl();
if (datasets.length) {
  sqlBodyEl.value = `SELECT * FROM "${datasets[0].name.replace(/"/g, '""')}" LIMIT 100`;
}
if (!sqlEnabled) sqlDisabledEl.classList.remove("d-none");

// ── mode toggle ─────────────────────────────────────────────────────────────
function setMode(mode) {
  const json = mode === "json";
  jsonModeEl.classList.toggle("d-none", !json);
  sqlModeEl.classList.toggle("d-none", json);
  clearMessages();
}
el("api-mode-json").addEventListener("change", () => setMode("json"));
el("api-mode-sql").addEventListener("change", () => setMode("sql"));

// ── JSON validate / prettify ────────────────────────────────────────────────
function prettifyJson() {
  clearMessages();
  const raw = jsonBodyEl.value.trim();
  if (!raw) {
    jsonBodyEl.value = sampleJsonBody();
    showCheck("Empty body — inserted a sample QueryRequest.", "ok");
    return null;
  }
  try {
    const parsed = JSON.parse(raw);
    jsonBodyEl.value = JSON.stringify(parsed, null, 2);
    showCheck("Valid JSON ✓", "ok");
    return parsed;
  } catch (e) {
    showCheck(`Invalid JSON: ${e.message}`, "warn");
    return null;
  }
}

// ── SQL syntax check (lightweight, read-only oriented) ───────────────────────
// Not a full parser: strips comments + string/identifier literals, then checks
// balanced parens, quote termination, single statement and read-only intent.
const WRITE_KEYWORDS = [
  "insert", "update", "delete", "merge", "drop", "alter", "create", "truncate",
  "attach", "detach", "copy", "install", "load", "pragma", "set", "call",
  "grant", "revoke", "vacuum", "export", "import",
];

function checkSql(sql) {
  const issues = [];
  let stripped = "";
  let i = 0;
  let quote = null; // "'" or '"'
  let unterminated = false;
  while (i < sql.length) {
    const c = sql[i];
    const next = sql[i + 1];
    if (quote) {
      if (c === quote) {
        if (next === quote) { i += 2; continue; } // escaped quote
        quote = null;
        i += 1;
        continue;
      }
      i += 1;
      continue;
    }
    if (c === "-" && next === "-") {
      while (i < sql.length && sql[i] !== "\n") i += 1;
      continue;
    }
    if (c === "/" && next === "*") {
      i += 2;
      while (i < sql.length && !(sql[i] === "*" && sql[i + 1] === "/")) i += 1;
      if (i >= sql.length) { issues.push("unterminated /* block comment */"); break; }
      i += 2;
      continue;
    }
    if (c === "'" || c === '"') { quote = c; i += 1; continue; }
    stripped += c;
    i += 1;
  }
  if (quote) unterminated = true;
  if (unterminated) issues.push(`unterminated ${quote === "'" ? "string" : "quoted identifier"} literal`);

  // Balanced parentheses (literals already removed).
  let depth = 0;
  for (const c of stripped) {
    if (c === "(") depth += 1;
    else if (c === ")") { depth -= 1; if (depth < 0) { issues.push("unbalanced ')' — too many closing parens"); break; } }
  }
  if (depth > 0) issues.push(`unbalanced '(' — ${depth} unclosed paren(s)`);

  // Single statement: at most one non-empty, non-trailing ';'.
  const statements = stripped.split(";").map((s) => s.trim()).filter(Boolean);
  if (statements.length > 1) issues.push("multiple statements — send a single query");

  const head = (statements[0] || stripped).trim().toLowerCase();
  if (!head) issues.push("empty statement");
  const firstWord = head.split(/\s+/)[0] || "";
  if (firstWord && WRITE_KEYWORDS.includes(firstWord)) {
    issues.push(`'${firstWord.toUpperCase()}' is not allowed — only read-only SELECT/WITH queries`);
  } else if (firstWord && !["select", "with", "from", "describe", "explain", "values", "table", "show"].includes(firstWord)) {
    issues.push(`unexpected leading keyword '${firstWord}' — expected SELECT or WITH`);
  }

  return issues;
}

function runSqlCheck() {
  clearMessages();
  const sql = sqlBodyEl.value.trim();
  if (!sql) { showCheck("Nothing to check — the SQL box is empty.", "warn"); return false; }
  const issues = checkSql(sql);
  if (issues.length) {
    showCheck("Issues found:\n  • " + issues.join("\n  • "), "warn");
    return false;
  }
  showCheck("Looks like a valid read-only query ✓ (server still validates on run)", "ok");
  return true;
}

// ── table rendering ──────────────────────────────────────────────────────────
function columnsOf(rows) {
  const seen = new Set();
  const cols = [];
  for (const r of rows) {
    if (r && typeof r === "object") {
      for (const k of Object.keys(r)) {
        if (!seen.has(k)) { seen.add(k); cols.push(k); }
      }
    }
  }
  return cols;
}

function cellText(v) {
  if (v == null) return "";
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

function renderTable(rows) {
  lastRows = Array.isArray(rows) ? rows : [];
  lastCols = columnsOf(lastRows);
  if (!lastRows.length) {
    resEl.innerHTML = '<div class="text-secondary">No rows.</div>';
    exportEl.classList.add("d-none");
    return;
  }
  let html = '<table class="table table-sm table-striped result-table"><thead><tr>';
  for (const c of lastCols) html += `<th>${escapeHtml(c)}</th>`;
  html += "</tr></thead><tbody>";
  for (const r of lastRows) {
    html += "<tr>";
    for (const c of lastCols) html += `<td>${escapeHtml(cellText(r ? r[c] : ""))}</td>`;
    html += "</tr>";
  }
  html += `</tbody></table><div class="text-secondary small">${lastRows.length} row(s)</div>`;
  resEl.innerHTML = html;
  exportEl.classList.remove("d-none");
}

// ── run against the API ──────────────────────────────────────────────────────
async function runJson() {
  clearMessages();
  exportEl.classList.add("d-none");
  let parsed;
  try {
    parsed = jsonBodyEl.value.trim() ? JSON.parse(jsonBodyEl.value) : {};
  } catch (e) {
    showError(`Invalid JSON body: ${e.message}`);
    return;
  }
  const name = selectedDataset();
  if (!name) { showError("No dataset selected."); return; }
  const url = `${apiBase}/datasets/${encodeURIComponent(name)}/query`;
  await runRequest(url, parsed);
}

async function runSql() {
  clearMessages();
  exportEl.classList.add("d-none");
  const sql = sqlBodyEl.value.trim();
  if (!sql) { showError("The SQL box is empty."); return; }
  const maxRows = parseInt(sqlMaxRowsEl.value, 10);
  const body = { sql };
  if (Number.isFinite(maxRows) && maxRows > 0) body.max_rows = maxRows;
  await runRequest(`${apiBase}/sql`, body);
}

async function runRequest(url, body) {
  resEl.innerHTML = '<span class="spinner-border spinner-border-sm"></span> Running…';
  setStatus("running…", "warning");
  clearTiming();
  const wantArrow = formatArrowEl.checked;
  const reqUrl = wantArrow ? `${url}${url.includes("?") ? "&" : "?"}format=arrow` : url;
  const t0 = performance.now();
  try {
    const resp = await fetch(reqUrl, {
      method: "POST",
      headers: buildHeaders(wantArrow),
      credentials: "same-origin",
      body: JSON.stringify(body),
    });
    const ttfb = performance.now() - t0;
    const clen = parseInt(resp.headers.get("content-length") || "", 10);

    if (!resp.ok) {
      const text = await resp.text();
      const total = performance.now() - t0;
      const bytes = Number.isFinite(clen) ? clen : new Blob([text]).size;
      resEl.innerHTML = "";
      let msg = text;
      try { msg = JSON.stringify(JSON.parse(text), null, 2); } catch { /* keep raw */ }
      showError(`HTTP ${resp.status} ${resp.statusText}\n${msg}`);
      showTiming({ ttfb, total, bytes, rows: null });
      setStatus("error", "danger");
      return;
    }

    const ctype = (resp.headers.get("content-type") || "").toLowerCase();
    const isArrow = ctype.includes("arrow");
    let rows;
    let bytes;
    let total;
    if (isArrow) {
      const buf = await resp.arrayBuffer();
      total = performance.now() - t0;
      bytes = Number.isFinite(clen) ? clen : buf.byteLength;
      const Arrow = await ensureArrow();
      const table = Arrow.tableFromIPC(new Uint8Array(buf));
      rows = table.toArray().map((r) => r.toJSON());
    } else {
      const text = await resp.text();
      total = performance.now() - t0;
      bytes = Number.isFinite(clen) ? clen : new Blob([text]).size;
      const json = text ? JSON.parse(text) : {};
      rows = Array.isArray(json.data) ? json.data : Array.isArray(json) ? json : [];
    }
    renderTable(rows);
    showTiming({ ttfb, total, bytes, rows: rows.length, format: isArrow ? "Arrow IPC" : "JSON" });
    setStatus(`${rows.length} row(s)`, "success");
  } catch (e) {
    resEl.innerHTML = "";
    showError(String(e));
    setStatus("error", "danger");
  }
}

// ── export: CSV / JSON / Parquet ─────────────────────────────────────────────
function downloadBlob(data, filename, mime) {
  const blob = data instanceof Blob ? data : new Blob([data], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

function exportTimestamp() {
  return new Date().toISOString().replace(/[:.]/g, "-").replace("T", "_").slice(0, 19);
}

function csvCell(v) {
  if (v == null) return "";
  if (typeof v === "bigint") v = v.toString();
  else if (typeof v === "object") v = JSON.stringify(v);
  const s = String(v);
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function exportCsv() {
  if (!lastRows.length) return;
  const lines = [lastCols.map(csvCell).join(",")];
  for (const r of lastRows) lines.push(lastCols.map((c) => csvCell(r ? r[c] : "")).join(","));
  downloadBlob(lines.join("\r\n"), `api-export_${exportTimestamp()}.csv`, "text/csv;charset=utf-8");
}

function exportJson() {
  if (!lastRows.length) return;
  const normalized = lastRows.map((r) => {
    const o = {};
    for (const c of lastCols) {
      const v = r ? r[c] : null;
      o[c] = typeof v === "bigint" ? v.toString() : v;
    }
    return o;
  });
  downloadBlob(JSON.stringify(normalized, null, 2), `api-export_${exportTimestamp()}.json`, "application/json");
}

// Locally-vendored DuckDB-WASM, booted lazily, only to write Parquet.
let duckReady = null;
async function ensureDuck() {
  if (duckReady) return duckReady;
  duckReady = (async () => {
    const base = new URL(`${explorerBase}/assets/vendor/duckdb/`, window.location.origin);
    const v = (file) => new URL(file, base).href;
    const duckdb = await import(v("duckdb-browser.bundled.mjs"));
    const bundle = await duckdb.selectBundle({
      mvp: { mainModule: v("duckdb-mvp.wasm"), mainWorker: v("duckdb-browser-mvp.worker.js") },
      eh: { mainModule: v("duckdb-eh.wasm"), mainWorker: v("duckdb-browser-eh.worker.js") },
    });
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker}");`], { type: "text/javascript" })
    );
    const worker = new Worker(workerUrl);
    const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
    const db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    URL.revokeObjectURL(workerUrl);
    const conn = await db.connect();
    return { db, conn };
  })();
  return duckReady;
}

async function exportParquet() {
  if (!lastRows.length) return;
  setStatus("encoding parquet…", "warning");
  const jsonFile = `api_rows_${Date.now()}.json`;
  const outFile = `api_export_${Date.now()}.parquet`;
  try {
    const { db, conn } = await ensureDuck();
    const payload = lastRows.map((r) => {
      const o = {};
      for (const c of lastCols) {
        const val = r ? r[c] : null;
        o[c] = typeof val === "bigint" ? val.toString() : val;
      }
      return o;
    });
    await db.registerFileText(jsonFile, JSON.stringify(payload));
    await conn.query(
      `CREATE OR REPLACE TABLE _api_export AS SELECT * FROM read_json_auto('${jsonFile}')`
    );
    await conn.query(`COPY _api_export TO '${outFile}' (FORMAT parquet)`);
    const buf = await db.copyFileToBuffer(outFile);
    downloadBlob(
      new Blob([buf], { type: "application/vnd.apache.parquet" }),
      `api-export_${exportTimestamp()}.parquet`,
      "application/vnd.apache.parquet"
    );
    setStatus(`${lastRows.length} row(s)`, "success");
  } catch (e) {
    showError(`Parquet export failed: ${e}`);
    setStatus("error", "danger");
  } finally {
    try {
      const { db, conn } = await ensureDuck();
      await conn.query(`DROP TABLE IF EXISTS _api_export`);
      await db.dropFile(jsonFile);
      await db.dropFile(outFile);
    } catch { /* ignore cleanup errors */ }
  }
}

// ── wiring ───────────────────────────────────────────────────────────────────
datasetEl.addEventListener("change", updateJsonUrl);
el("api-json-prettify").addEventListener("click", prettifyJson);
el("api-json-sample").addEventListener("click", () => { jsonBodyEl.value = sampleJsonBody(); clearMessages(); });
el("api-json-run").addEventListener("click", runJson);
el("api-sql-check").addEventListener("click", runSqlCheck);
el("api-sql-run").addEventListener("click", runSql);
el("api-export-csv").addEventListener("click", exportCsv);
el("api-export-json").addEventListener("click", exportJson);
el("api-export-parquet").addEventListener("click", exportParquet);

jsonBodyEl.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter") runJson();
});
sqlBodyEl.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter") runSql();
});
