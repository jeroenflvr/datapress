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
const compressOffEl = el("api-compress-off");

let lastRows = [];
let lastCols = [];

//  small helpers 
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

//  timing / size readout 
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

//  request headers (custom + format-derived Accept) 
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

//  populate dataset select + defaults 
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

//  mode toggle 
function setMode(mode) {
  const json = mode === "json";
  jsonModeEl.classList.toggle("d-none", !json);
  sqlModeEl.classList.toggle("d-none", json);
  clearMessages();
}
el("api-mode-json").addEventListener("change", () => setMode("json"));
el("api-mode-sql").addEventListener("change", () => setMode("sql"));

//  JSON validate / prettify 
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

//  SQL syntax check (lightweight, read-only oriented) 
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

//  SQL → structured QueryRequest translator 
// Best-effort conversion of the SQL subset the structured /query endpoint can
// express: a single table, ANDed predicates, optional group_by + simple
// aggregates, order_by and limit. Anything outside that subset throws an Error
// whose message explains what couldn't be represented. Handy when the raw-SQL
// endpoint is disabled but you still want a runnable request body.
const AGG_FUNCS = ["count", "sum", "avg", "min", "max"];
const UNSUPPORTED_KEYWORDS = {
  join: "joins aren't supported — the structured query reads a single dataset",
  inner: "joins aren't supported — the structured query reads a single dataset",
  left: "joins aren't supported — the structured query reads a single dataset",
  right: "joins aren't supported — the structured query reads a single dataset",
  full: "joins aren't supported — the structured query reads a single dataset",
  cross: "joins aren't supported — the structured query reads a single dataset",
  union: "UNION/INTERSECT/EXCEPT can't be expressed as a structured query",
  intersect: "UNION/INTERSECT/EXCEPT can't be expressed as a structured query",
  except: "UNION/INTERSECT/EXCEPT can't be expressed as a structured query",
  having: "HAVING requires GROUP BY",
  over: "window functions can't be expressed as a structured query",  offset: "OFFSET isn't supported — use page / page_size in the JSON body",
};

function tokenizeSqlStmt(sql) {
  const tokens = [];
  let i = 0;
  const n = sql.length;
  while (i < n) {
    const c = sql[i];
    if (/\s/.test(c)) { i += 1; continue; }
    if (c === "-" && sql[i + 1] === "-") { while (i < n && sql[i] !== "\n") i += 1; continue; }
    if (c === "/" && sql[i + 1] === "*") {
      i += 2;
      while (i < n && !(sql[i] === "*" && sql[i + 1] === "/")) i += 1;
      if (i >= n) throw new Error("unterminated /* block comment */");
      i += 2; continue;
    }
    if (c === "'") {
      let j = i + 1, s = "";
      while (j < n) {
        if (sql[j] === "'") { if (sql[j + 1] === "'") { s += "'"; j += 2; continue; } break; }
        s += sql[j]; j += 1;
      }
      if (j >= n) throw new Error("unterminated string literal");
      tokens.push({ t: "str", v: s }); i = j + 1; continue;
    }
    if (c === '"') {
      let j = i + 1, s = "";
      while (j < n) {
        if (sql[j] === '"') { if (sql[j + 1] === '"') { s += '"'; j += 2; continue; } break; }
        s += sql[j]; j += 1;
      }
      if (j >= n) throw new Error("unterminated quoted identifier");
      tokens.push({ t: "id", v: s, quoted: true }); i = j + 1; continue;
    }
    if (/[0-9]/.test(c) || (c === "." && /[0-9]/.test(sql[i + 1] || ""))) {
      let j = i;
      while (j < n && /[0-9.]/.test(sql[j])) j += 1;
      if (sql[j] === "e" || sql[j] === "E") {
        j += 1;
        if (sql[j] === "+" || sql[j] === "-") j += 1;
        while (j < n && /[0-9]/.test(sql[j])) j += 1;
      }
      tokens.push({ t: "num", v: sql.slice(i, j) }); i = j; continue;
    }
    const two = sql.slice(i, i + 2);
    if (["<=", ">=", "<>", "!="].includes(two)) { tokens.push({ t: "op", v: two === "!=" ? "<>" : two }); i += 2; continue; }
    if ("=<>(),*.;+-".includes(c)) { tokens.push({ t: "op", v: c }); i += 1; continue; }
    if (/[A-Za-z_]/.test(c)) {
      let j = i;
      while (j < n && /[A-Za-z0-9_]/.test(sql[j])) j += 1;
      tokens.push({ t: "id", v: sql.slice(i, j) }); i = j; continue;
    }
    throw new Error(`unexpected character '${c}'`);
  }
  return tokens;
}

function sqlToQueryRequest(sql) {
  const trimmed = sql.trim().replace(/;+\s*$/, "");
  if (!trimmed) throw new Error("the SQL box is empty");
  const toks = tokenizeSqlStmt(trimmed);
  if (toks.some((t) => t.t === "op" && t.v === ";")) {
    throw new Error("only a single statement can be translated");
  }

  let p = 0;
  const peek = (o = 0) => toks[p + o];
  const isKw = (tk, w) => tk && tk.t === "id" && !tk.quoted && tk.v.toLowerCase() === w;
  const atKw = (w, o = 0) => isKw(peek(o), w);
  const isOp = (v, o = 0) => { const tk = peek(o); return tk && tk.t === "op" && tk.v === v; };
  const eat = () => toks[p++];
  const expectOp = (v) => { if (!isOp(v)) throw new Error(`expected '${v}'`); p += 1; };
  const ident = () => {
    const tk = peek();
    if (!tk || tk.t !== "id") throw new Error("expected an identifier");
    p += 1;
    if (isOp(".")) throw new Error("table-qualified columns (t.col) aren't supported");
    return tk.v;
  };

  const notes = [];
  const columns = [];
  const aggregations = [];
  const aggSignatures = new Map(); // "op:col" -> effective output alias
  const predicates = [];
  const having = [];
  const groupBy = [];
  const orderBy = [];
  let star = false;
  let distinct = false;
  let limit = null;

  const parseValue = () => {
    let sign = 1;
    if (isOp("-")) { sign = -1; eat(); } else if (isOp("+")) eat();
    const v = peek();
    if (!v) throw new Error("expected a value");
    if (v.t === "num") { eat(); return sign * Number(v.v); }
    if (v.t === "str") { if (sign === -1) throw new Error("unexpected '-' before a string"); eat(); return v.v; }
    if (v.t === "id" && !v.quoted) {
      const low = v.v.toLowerCase();
      if (low === "true") { eat(); return true; }
      if (low === "false") { eat(); return false; }
      if (low === "null") { eat(); return null; }
    }
    throw new Error("expected a literal value (string, number, true/false/null)");
  };

  const parsePredicate = () => {
    const col = ident();
    if (atKw("is")) {
      eat();
      if (atKw("not")) {
        eat();
        if (!atKw("null")) throw new Error("expected NULL after IS NOT");
        eat(); predicates.push({ col, op: "is_not_null" }); return;
      }
      if (!atKw("null")) throw new Error("expected NULL after IS");
      eat(); predicates.push({ col, op: "is_null" }); return;
    }
    let negate = false;
    if (atKw("not")) { negate = true; eat(); }
    if (atKw("like") || atKw("ilike")) {
      const op = eat().v.toLowerCase();
      if (negate) throw new Error(`NOT ${op.toUpperCase()} isn't supported by the structured API`);
      predicates.push({ col, op, val: parseValue() }); return;
    }
    if (atKw("in")) {
      if (negate) throw new Error("NOT IN isn't supported by the structured API");
      eat(); expectOp("(");
      const vals = [parseValue()];
      while (isOp(",")) { eat(); vals.push(parseValue()); }
      expectOp(")");
      predicates.push({ col, op: "in", val: vals }); return;
    }
    if (atKw("between")) {
      if (negate) throw new Error("NOT BETWEEN isn't supported by the structured API");
      eat();
      const lo = parseValue();
      if (!atKw("and")) throw new Error("expected AND in BETWEEN");
      eat();
      const hi = parseValue();
      predicates.push({ col, op: "gte", val: lo });
      predicates.push({ col, op: "lte", val: hi });
      notes.push(`expanded '${col} BETWEEN …' into gte + lte predicates`);
      return;
    }
    if (negate) throw new Error("NOT before this operator isn't supported");
    const tk = peek();
    if (!tk || tk.t !== "op") throw new Error(`expected a comparison operator after '${col}'`);
    const opMap = { "=": "eq", "<>": "neq", ">": "gt", ">=": "gte", "<": "lt", "<=": "lte" };
    const mapped = opMap[tk.v];
    if (!mapped) throw new Error(`unsupported operator '${tk.v}'`);
    eat();
    predicates.push({ col, op: mapped, val: parseValue() });
  };

  const parsePredicateList = () => {
    parsePredicate();
    while (atKw("and")) { eat(); parsePredicate(); }
    if (atKw("or")) throw new Error("OR in WHERE can't be expressed — predicates are ANDed only");
  };

  // HAVING references an aggregation (by its SELECT expression) or a group
  // column. Aggregate expressions are matched back to the alias the
  // structured request will expose, so `HAVING COUNT(*) > 5` becomes a
  // predicate on that aggregation's alias.
  const parseHavingItem = () => {
    const tk = peek();
    let col;
    if (tk && tk.t === "id" && !tk.quoted && AGG_FUNCS.includes(tk.v.toLowerCase()) && isOp("(", 1)) {
      const op = tk.v.toLowerCase(); eat(); expectOp("(");
      let acol = null;
      if (isOp("*")) { eat(); if (op !== "count") throw new Error(`${op.toUpperCase()}(*) isn't valid — use a column`); }
      else {
        if (atKw("distinct")) throw new Error("COUNT(DISTINCT …) isn't supported by the structured API");
        acol = ident();
      }
      expectOp(")");
      const sig = `${op}:${acol == null ? "*" : acol.toLowerCase()}`;
      col = aggSignatures.get(sig);
      if (!col) throw new Error(`HAVING uses ${op.toUpperCase()}(${acol ?? "*"}) which isn't in the SELECT list — add it as an aggregation first`);
    } else {
      col = ident(); // an aggregation alias or a group column
    }
    const optk = peek();
    if (!optk || optk.t !== "op") throw new Error(`expected a comparison operator in HAVING after '${col}'`);
    const opMap = { "=": "eq", "<>": "neq", ">": "gt", ">=": "gte", "<": "lt", "<=": "lte" };
    const mapped = opMap[optk.v];
    if (!mapped) throw new Error(`unsupported operator '${optk.v}' in HAVING`);
    eat();
    having.push({ col, op: mapped, val: parseValue() });
  };

  const parseHavingList = () => {
    parseHavingItem();
    while (atKw("and")) { eat(); parseHavingItem(); }
    if (atKw("or")) throw new Error("OR in HAVING can't be expressed — predicates are ANDed only");
  };

  const parseSelectItem = () => {
    if (isOp("*")) { eat(); star = true; return; }
    const tk = peek();
    if (tk && tk.t === "id" && !tk.quoted && AGG_FUNCS.includes(tk.v.toLowerCase()) && isOp("(", 1)) {
      const op = tk.v.toLowerCase(); eat(); expectOp("(");
      let col = null;
      if (isOp("*")) { eat(); if (op !== "count") throw new Error(`${op.toUpperCase()}(*) isn't valid — use a column`); }
      else {
        if (atKw("distinct")) throw new Error("COUNT(DISTINCT …) isn't supported by the structured API");
        col = ident();
      }
      expectOp(")");
      if (atKw("over")) throw new Error("window functions can't be expressed as a structured query");
      const agg = { op };
      if (col != null) agg.col = col;
      if (atKw("as")) { eat(); agg.alias = ident(); }
      else if (peek() && peek().t === "id" && !atKw("from")) { agg.alias = ident(); }
      aggregations.push(agg);
      const effAlias = agg.alias || (col == null ? "count" : `${op}_${col.toLowerCase()}`);
      aggSignatures.set(`${op}:${col == null ? "*" : col.toLowerCase()}`, effAlias);
      return;
    }
    const col = ident();
    if (atKw("as") || (peek() && peek().t === "id" && !atKw("from"))) {
      throw new Error(`column alias on '${col}' isn't supported — the structured API returns names verbatim`);
    }
    columns.push(col);
  };

  if (atKw("with")) throw new Error("CTEs (WITH …) can't be expressed as a structured query");
  if (!atKw("select")) throw new Error("only SELECT statements can be translated");
  eat();
  if (atKw("distinct")) { distinct = true; eat(); } else if (atKw("all")) eat();

  parseSelectItem();
  while (isOp(",")) { eat(); parseSelectItem(); }

  if (!atKw("from")) throw new Error("expected FROM");
  eat();
  const ftk = peek();
  if (!ftk || ftk.t !== "id") throw new Error("expected a table name after FROM");
  const table = ftk.v; eat();
  if (isOp(".")) throw new Error("schema-qualified tables aren't supported");
  if (isOp(",")) throw new Error("joins aren't supported — the structured query reads a single dataset");
  if (peek() && peek().t === "id") {
    const w = peek().v.toLowerCase();
    if (UNSUPPORTED_KEYWORDS[w]) throw new Error(UNSUPPORTED_KEYWORDS[w]);
    if (!["where", "group", "order", "limit"].includes(w)) {
      throw new Error(`table alias '${peek().v}' isn't supported — drop it`);
    }
  }

  if (atKw("where")) { eat(); parsePredicateList(); }

  if (atKw("group")) {
    eat();
    if (!atKw("by")) throw new Error("expected BY after GROUP");
    eat();
    groupBy.push(ident());
    while (isOp(",")) { eat(); groupBy.push(ident()); }
  }
  if (atKw("having")) { eat(); parseHavingList(); }

  if (atKw("order")) {
    eat();
    if (!atKw("by")) throw new Error("expected BY after ORDER");
    eat();
    const parseSort = () => {
      const col = ident();
      const o = { col };
      if (atKw("asc")) { o.dir = "asc"; eat(); } else if (atKw("desc")) { o.dir = "desc"; eat(); }
      orderBy.push(o);
    };
    parseSort();
    while (isOp(",")) { eat(); parseSort(); }
  }

  if (atKw("limit")) {
    eat();
    const v = peek();
    if (!v || v.t !== "num") throw new Error("expected a number after LIMIT");
    eat();
    limit = Math.trunc(Number(v.v));
    if (atKw("offset")) throw new Error("OFFSET isn't supported — use page / page_size in the JSON body");
  }

  if (p < toks.length) {
    const lt = peek();
    const w = lt.t === "id" ? lt.v.toLowerCase() : lt.v;
    if (UNSUPPORTED_KEYWORDS[w]) throw new Error(UNSUPPORTED_KEYWORDS[w]);
    throw new Error(`unexpected '${lt.v}' — this part can't be translated`);
  }

  const hasAgg = aggregations.length > 0;
  if (hasAgg && groupBy.length === 0) {
    throw new Error("aggregates require GROUP BY in the structured API (use the /count endpoint for a bare COUNT(*))");
  }
  if (hasAgg && distinct) throw new Error("DISTINCT can't be combined with aggregates here");

  const request = {};
  if (groupBy.length) {
    request.group_by = groupBy;
    if (aggregations.length) request.aggregations = aggregations;
    if (having.length) request.having = having;
    const extra = columns.filter((c) => !groupBy.some((g) => g.toLowerCase() === c.toLowerCase()));
    if (extra.length) notes.push(`ignored non-grouped select column(s): ${extra.join(", ")}`);
    if (star) notes.push("ignored '*' — group_by drives the projection");
  } else {
    if (having.length) throw new Error("HAVING requires GROUP BY");
    if (distinct) request.distinct = true;
    if (!star && columns.length) request.columns = columns;
  }
  if (predicates.length) request.predicates = predicates;
  if (orderBy.length) request.order_by = orderBy;
  if (limit != null) { request.limit = limit; request.page_size = Math.max(1, limit); }

  return { request, table, notes };
}

function translateSqlToJson() {
  clearMessages();
  let result;
  try {
    result = sqlToQueryRequest(sqlBodyEl.value);
  } catch (e) {
    showCheck(`Can't translate to a structured query:\n  • ${e.message}`, "warn");
    return;
  }
  const { request, table, notes } = result;
  const match = datasets.find((d) => d.name.toLowerCase() === table.toLowerCase());
  if (match) { datasetEl.value = match.name; updateJsonUrl(); }
  else notes.unshift(`dataset '${table}' isn't registered here — pick one from the dropdown`);

  jsonBodyEl.value = JSON.stringify(request, null, 2);
  el("api-mode-json").checked = true;
  setMode("json");
  const tail = notes.length ? "\n  • " + notes.join("\n  • ") : "";
  showCheck(`Translated to a structured query ✓ — switched to JSON mode.${tail}`, "ok");
}

//  table rendering 
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

//  run against the API 
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
  // Browsers can't set Accept-Encoding from a page, so request the
  // server-side compression bypass via a query param instead.
  const noCompress = compressOffEl && compressOffEl.checked;
  const params = [];
  if (wantArrow) params.push("format=arrow");
  if (noCompress) params.push("compress=false");
  const reqUrl = params.length
    ? `${url}${url.includes("?") ? "&" : "?"}${params.join("&")}`
    : url;
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

//  export: CSV / JSON / Parquet 
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

//  wiring 
datasetEl.addEventListener("change", updateJsonUrl);
el("api-json-prettify").addEventListener("click", prettifyJson);
el("api-json-sample").addEventListener("click", () => { jsonBodyEl.value = sampleJsonBody(); clearMessages(); });
el("api-json-run").addEventListener("click", runJson);
el("api-sql-check").addEventListener("click", runSqlCheck);
el("api-sql-tojson").addEventListener("click", translateSqlToJson);
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
