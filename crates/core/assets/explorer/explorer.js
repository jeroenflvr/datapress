// DataPress explorer console. Served at {explorer_base}/assets/explorer.js and
// embedded in the binary via include_str!. Runtime data (dataset list, mount
// path) is read from inline <script type="application/json"> config blocks so
// this file stays free of server-side templating.
import * as duckdb from "https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@1.29.0/+esm";

const config = JSON.parse(document.getElementById("explorer-config").textContent || "{}");
const datasets = JSON.parse(document.getElementById("datasets-data").textContent || "[]");
const statusEl = document.getElementById("duck-status");
const selectEl = document.getElementById("duck-dataset");
const sqlEl = document.getElementById("duck-sql");
const runBtn = document.getElementById("duck-run");
const errEl = document.getElementById("duck-error");
const resEl = document.getElementById("duck-results");

for (const d of datasets) {
  const opt = document.createElement("option");
  opt.value = d.name;
  opt.textContent = `${d.name} (${d.rows} rows)`;
  selectEl.appendChild(opt);
}

const setStatus = (text, cls) => {
  statusEl.textContent = text;
  statusEl.className = `badge ms-auto text-bg-${cls || "secondary"}`;
};

let conn = null;
let registered = new Set();

async function initDuck() {
  setStatus("loading engine…", "secondary");
  const bundles = duckdb.getJsDelivrBundles();
  const bundle = await duckdb.selectBundle(bundles);
  const worker_url = URL.createObjectURL(
    new Blob([`importScripts("${bundle.mainWorker}");`], { type: "text/javascript" })
  );
  const worker = new Worker(worker_url);
  const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
  const db = new duckdb.AsyncDuckDB(logger, worker);
  await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
  URL.revokeObjectURL(worker_url);
  conn = await db.connect();
  setStatus("ready", "success");
  runBtn.disabled = false;
  if (datasets.length) loadSampleQuery(datasets[0].name);
}

async function ensureView(name) {
  if (registered.has(name)) return;
  const d = datasets.find((x) => x.name === name);
  if (!d) throw new Error(`unknown dataset: ${name}`);
  const url = new URL(d.parquet, window.location.origin).href;
  const ident = name.replace(/"/g, '""');
  await conn.query(
    `CREATE OR REPLACE VIEW "${ident}" AS SELECT * FROM read_parquet('${url.replace(/'/g, "''")}')`
  );
  registered.add(name);
}

function renderTable(table) {
  const rows = table.toArray().map((r) => r.toJSON());
  if (!rows.length) {
    resEl.innerHTML = '<div class="text-secondary">No rows.</div>';
    return;
  }
  const cols = table.schema.fields.map((f) => f.name);
  let html = '<table class="table table-sm table-striped result-table"><thead><tr>';
  for (const c of cols) html += `<th>${escapeHtml(c)}</th>`;
  html += "</tr></thead><tbody>";
  for (const r of rows) {
    html += "<tr>";
    for (const c of cols) {
      let v = r[c];
      if (typeof v === "bigint") v = v.toString();
      else if (v && typeof v === "object") v = JSON.stringify(v);
      html += `<td>${escapeHtml(v == null ? "" : String(v))}</td>`;
    }
    html += "</tr>";
  }
  html += `</tbody></table><div class="text-secondary small">${rows.length} row(s)</div>`;
  resEl.innerHTML = html;
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

async function runQuery() {
  if (!conn) return;
  errEl.classList.add("d-none");
  resEl.innerHTML = '<span class="spinner-border spinner-border-sm"></span> Running…';
  setStatus("running…", "warning");
  try {
    for (const d of datasets) {
      if (sqlEl.value.includes(d.name)) await ensureView(d.name);
    }
    const table = await conn.query(sqlEl.value);
    renderTable(table);
    setStatus("ready", "success");
  } catch (e) {
    resEl.innerHTML = "";
    errEl.textContent = String(e);
    errEl.classList.remove("d-none");
    setStatus("error", "danger");
  }
}

function loadSampleQuery(name) {
  const ident = `"${name.replace(/"/g, '""')}"`;
  sqlEl.value = `SELECT * FROM ${ident} LIMIT 100`;
}

runBtn.addEventListener("click", runQuery);
sqlEl.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter") runQuery();
});
document.getElementById("duck-count").addEventListener("click", () => {
  const ident = `"${selectEl.value.replace(/"/g, '""')}"`;
  sqlEl.value = `SELECT count(*) AS rows FROM ${ident}`;
  runQuery();
});
document.getElementById("duck-sample").addEventListener("click", () => {
  loadSampleQuery(selectEl.value);
  runQuery();
});
document.getElementById("duck-describe").addEventListener("click", () => {
  const ident = `"${selectEl.value.replace(/"/g, '""')}"`;
  sqlEl.value = `DESCRIBE SELECT * FROM ${ident}`;
  runQuery();
});
selectEl.addEventListener("change", () => loadSampleQuery(selectEl.value));

// Lazily initialise DuckDB the first time the tab is shown.
let booted = false;
document.getElementById("duckdb-tab").addEventListener("shown.bs.tab", () => {
  if (booted) return;
  booted = true;
  initDuck().catch((e) => {
    setStatus("failed", "danger");
    errEl.textContent = String(e);
    errEl.classList.remove("d-none");
  });
});

// Lazily load the embedded shell terminal the first time its tab is shown.
const termFrame = document.getElementById("terminal-frame");
document.getElementById("terminal-tab").addEventListener("shown.bs.tab", () => {
  if (!termFrame.getAttribute("src")) {
    termFrame.src = `${config.explorerBase}/terminal`;
  }
});
