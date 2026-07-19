/**
 * manage.js — Phase 6B explorer integration:
 *   R8.7  Save as dataset (query view)
 *   R8.8  Admin-token prompting (tab memory only, never stored)
 *   R8.9  Manage view (state badges, delete action)
 *   R8.12 Per-dataset reload button + Reload-all
 */

(function () {
  "use strict";

  // -----------------------------------------------------------------------
  // Config read from the server-rendered <script id="explorer-config"> block
  // -----------------------------------------------------------------------
  let CFG = {};
  try {
    const el = document.getElementById("explorer-config");
    if (el) CFG = JSON.parse(el.textContent || el.innerText);
  } catch (e) { /* ignore */ }

  const API_BASE = CFG.apiBase || "/api/v1";
  const QUERIES_ENABLED = Boolean(CFG.queriesEnabled);
  const STORAGE_BACKEND = CFG.storageBackend || "";   // "" = no storage

  // -----------------------------------------------------------------------
  // Admin token — held in a module-level variable only (R8.8).
  // Never written to localStorage, sessionStorage, DOM attributes, or URLs.
  // -----------------------------------------------------------------------
  let _adminToken = "";   // cleared on 403

  function getAdminToken() { return _adminToken; }
  function setAdminToken(t) { _adminToken = t.trim(); }
  function clearAdminToken() { _adminToken = ""; }

  // Build the auth headers for an API call.
  function authHeaders() {
    const h = { "Content-Type": "application/json" };
    if (_adminToken) h["X-Admin-Token"] = _adminToken;
    // Reuse the OIDC bearer token if the user signed in via the API Query tab.
    try {
      const raw = sessionStorage.getItem("datapress.explorer.oauth2.token");
      if (raw) {
        const rec = JSON.parse(raw);
        if (rec && rec.accessToken) {
          h["Authorization"] = "Bearer " + rec.accessToken;
        }
      }
    } catch (e) { /* ignore */ }
    return h;
  }

  // Prompt for the admin token (shown inline in the save form, or via prompt).
  // Returns false if the user cancelled.
  function ensureToken(tokenInputId) {
    if (_adminToken) return true;
    const inputEl = tokenInputId ? document.getElementById(tokenInputId) : null;
    const tokenVal = inputEl ? inputEl.value.trim() : "";
    if (tokenVal) {
      setAdminToken(tokenVal);
      return true;
    }
    // Fallback: prompt() — acceptable since it's an admin action.
    const t = window.prompt("Enter admin token (X-Admin-Token):");
    if (t === null) return false;   // user cancelled
    setAdminToken(t);
    return true;
  }

  // -----------------------------------------------------------------------
  // Generic fetch wrapper that clears the token on 403.
  // -----------------------------------------------------------------------
  async function apiFetch(url, opts) {
    const resp = await fetch(url, opts);
    if (resp.status === 403) clearAdminToken();
    return resp;
  }

  // -----------------------------------------------------------------------
  // R8.12 — Per-dataset reload
  // Exposed as window.dpReloadDataset so dataset.html inline onclick works.
  // -----------------------------------------------------------------------
  window.dpReloadDataset = async function dpReloadDataset(name, apiBase) {
    if (!ensureToken("save-admin-token")) return;
    const btn   = document.getElementById("reload-btn-" + name);
    const badge = document.getElementById("state-badge-" + name);

    function setBuilding() {
      if (btn)   { btn.disabled = true; btn.innerHTML = `<span class="spinner-border spinner-border-sm"></span>`; }
      if (badge) { badge.className = "badge text-bg-warning ms-1"; badge.textContent = "building"; }
    }
    function restore(btn) {
      if (btn) { btn.disabled = false; btn.innerHTML = `<i class="bi bi-arrow-clockwise"></i> Reload`; }
    }
    setBuilding();

    async function doReload() {
      const resp = await apiFetch(apiBase + "/datasets/" + encodeURIComponent(name) + "/reload", {
        method: "POST", headers: authHeaders(),
      });
      if (resp.status === 403) {
        if (!ensureToken("save-admin-token")) { restore(btn); return false; }
        const resp2 = await apiFetch(apiBase + "/datasets/" + encodeURIComponent(name) + "/reload", {
          method: "POST", headers: authHeaders(),
        });
        if (!resp2.ok) { alert("Reload failed: " + resp2.status); restore(btn); return false; }
      } else if (!resp.ok) {
        const text = await resp.text();
        alert("Reload failed: " + resp.status + "\n" + text);
        restore(btn);
        return false;
      }
      return true;
    }

    try {
      const ok = await doReload();
      if (!ok) return;
      // POST succeeded — the build is now in progress (async reload path).
      // Poll /status until the dataset reaches a terminal state, then
      // re-enable the button and surface the outcome.
      pollUntilTerminal(name, function (entry) {
        restore(btn);
        if (!entry) return;   // timed out
        const state = (entry.state || "").toLowerCase();
        if (badge) {
          if (state === "published") {
            badge.className = "badge text-bg-success ms-1";
            badge.textContent = "reloaded";
            setTimeout(function () { badge.className = ""; badge.textContent = ""; }, 4000);
          } else if (state === "failed") {
            badge.className = "badge text-bg-danger ms-1";
            badge.textContent = "failed";
            if (entry.last_error) badge.title = entry.last_error;
          }
        }
      });
    } catch (e) {
      alert("Reload error: " + e.message);
      restore(btn);
    }
  };

  // -----------------------------------------------------------------------
  // R8.9 — Delete managed dataset
  // -----------------------------------------------------------------------
  window.dpDeleteDataset = async function dpDeleteDataset(name, apiBase) {
    if (!window.confirm('Delete dataset "' + name + '"?\n\nThis cannot be undone.')) return;
    if (!ensureToken("save-admin-token")) return;
    const btn = document.getElementById("delete-btn-" + name);
    if (btn) btn.disabled = true;
    try {
      const resp = await apiFetch(apiBase + "/queries/" + encodeURIComponent(name), {
        method: "DELETE",
        headers: authHeaders(),
      });
      if (resp.status === 409) {
        const body = await resp.json().catch(function () { return {}; });
        const msg = (body && body.error) ? body.error : "Dataset has dependents — cannot delete.";
        alert(msg);
        return;
      }
      if (resp.status === 403) {
        alert("Forbidden: this dataset was not created via the saved-queries API.");
        return;
      }
      if (!resp.ok) {
        alert("Delete failed: " + resp.status);
        return;
      }
      // Remove the list row live (no full-page reload needed — the row is the
      // only DOM node that references this dataset in the discovery list).
      const list = document.querySelector(".dataset-list");
      const row = list && list.querySelector(
        '.list-group-item[data-name="' + CSS.escape(name) + '"]'
      );
      if (row) {
        row.remove();
      }
      // If the detail pane was showing this dataset, clear it.
      const detail = document.getElementById("dataset-detail");
      if (detail && detail.querySelector('[id="reload-btn-' + name + '"]')) {
        detail.innerHTML = '<div class="text-secondary p-4">Dataset deleted.</div>';
      }
    } catch (e) {
      alert("Delete error: " + e.message);
    } finally {
      if (btn) btn.disabled = false;
    }
  };

  // -----------------------------------------------------------------------
  // R8.12 — Reload-all button
  // -----------------------------------------------------------------------
  function initReloadAll() {
    const btn = document.getElementById("reload-all-btn");
    if (!btn) return;
    // Show the button when admin is available (queries or reload auth).
    btn.classList.remove("d-none");

    btn.addEventListener("click", async function () {
      // Count visible (non-hidden) datasets for the confirmation dialog.
      const items = Array.from(document.querySelectorAll(".dataset-list .list-group-item:not(.d-none)"));
      const count = items.length;
      if (!window.confirm("Reload all " + count + " dataset(s) in topological order?")) return;
      if (!ensureToken(null)) return;

      btn.disabled = true;
      btn.innerHTML = `<span class="spinner-border spinner-border-sm"></span> Reloading…`;

      try {
        const resp = await apiFetch(API_BASE + "/datasets/reload-all", {
          method: "POST",
          headers: authHeaders(),
        });
        if (resp.status === 403) {
          alert("Forbidden: admin token required.");
          return;
        }
        if (!resp.ok) {
          alert("Reload-all failed: " + resp.status);
          return;
        }
        const body = await resp.json();
        const enqueuedList = (body.enqueued || []);
        const skippedList  = (body.skipped  || []);
        const enqueued = enqueuedList.join(", ") || "none";
        const skipped  = skippedList.join(", ")  || "none";

        // Show a transient summary that auto-updates as builds complete.
        const existing = document.getElementById("reload-all-summary");
        if (existing) existing.remove();
        const summary = document.createElement("div");
        summary.id = "reload-all-summary";
        summary.className = "alert alert-info alert-dismissible mt-2 small";
        const closeBtn = `<button type="button" class="btn-close" data-bs-dismiss="alert" aria-label="Close"></button>`;
        function renderSummary(doneCount) {
          const prog = enqueuedList.length > 0
            ? " (" + doneCount + "/" + enqueuedList.length + " done)"
            : "";
          summary.innerHTML = `<strong>Reload-all:</strong> enqueued: ${enqueued}${prog} | skipped: ${skipped}${closeBtn}`;
        }
        renderSummary(0);
        btn.parentNode.insertAdjacentElement("afterend", summary);

        // Poll each enqueued dataset and update the summary + list-item rows.
        if (enqueuedList.length > 0) {
          let done = 0;
          enqueuedList.forEach(function (name) {
            pollUntilTerminal(name, function (entry) {
              done++;
              renderSummary(done);
              if (done >= enqueuedList.length) {
                // All done — remove summary after a short delay.
                setTimeout(function () { if (summary.parentNode) summary.remove(); }, 5000);
              }
            });
          });
        } else {
          setTimeout(function () { if (summary.parentNode) summary.remove(); }, 10000);
        }
      } catch (e) {
        alert("Reload-all error: " + e.message);
      } finally {
        btn.disabled = false;
        btn.innerHTML = `<i class="bi bi-arrow-repeat"></i> Reload all`;
      }
    });
  }

  // -----------------------------------------------------------------------
  // R8.7 — Save as dataset form
  // -----------------------------------------------------------------------
  function initSaveAsDataset() {
    if (!QUERIES_ENABLED) return;

    // Show the save-as-dataset toggle buttons next to both Run buttons.
    // Buttons carry class "save-as-dataset-btn-wrap" (one per mode toolbar).
    document.querySelectorAll(".save-as-dataset-btn-wrap").forEach(function (wrap) {
      wrap.classList.remove("d-none");
    });

    // Show storage residency/sort_by fields if storage is configured.
    const storageFields = document.getElementById("save-storage-fields");
    if (storageFields && STORAGE_BACKEND) {
      storageFields.classList.remove("d-none");
    }

    // Toggle TTL field based on kind selection.
    const kindSel = document.getElementById("save-kind");
    const ttlWrap = document.getElementById("save-ttl-wrap");
    if (kindSel && ttlWrap) {
      kindSel.addEventListener("change", function () {
        ttlWrap.classList.toggle("d-none", kindSel.value !== "temp");
      });
    }

    const submitBtn = document.getElementById("save-submit");
    if (!submitBtn) return;

    submitBtn.addEventListener("click", async function () {
      const nameEl = document.getElementById("save-name");
      const name = nameEl ? nameEl.value.trim() : "";
      if (!name) {
        if (nameEl) { nameEl.classList.add("is-invalid"); nameEl.focus(); }
        return;
      }
      if (nameEl) nameEl.classList.remove("is-invalid");

      // Read the current SQL from the API Query tab's SQL textarea.
      const sqlEl = document.getElementById("api-sql-body");
      const sql = sqlEl ? sqlEl.value.trim() : "";
      if (!sql) {
        alert("Write a SQL query in the API Query tab's SQL editor first.");
        return;
      }

      const tokenInput = document.getElementById("save-admin-token");
      if (tokenInput && tokenInput.value.trim()) {
        setAdminToken(tokenInput.value.trim());
      }
      if (!ensureToken("save-admin-token")) return;

      const kind = (document.getElementById("save-kind") || {}).value || "temp";
      const ttlVal = ((document.getElementById("save-ttl") || {}).value || "").trim();
      const interval = ((document.getElementById("save-interval") || {}).value || "").trim();
      const onUpstream = !!(document.getElementById("save-on-upstream") || {}).checked;
      const residency = ((document.getElementById("save-residency") || {}).value || "auto");
      const sortByRaw = ((document.getElementById("save-sort-by") || {}).value || "").trim();
      const sortBy = sortByRaw ? sortByRaw.split(",").map(function (s) { return s.trim(); }).filter(Boolean) : [];

      const payload = { name: name, sql: sql, kind: kind };
      if (kind === "temp" && ttlVal) payload.ttl = ttlVal;
      if (interval || onUpstream) {
        payload.refresh = {};
        if (interval) payload.refresh.interval = interval;
        if (onUpstream) payload.refresh.on_upstream_reload = true;
      }
      if (STORAGE_BACKEND) {
        payload.materialize = { residency: residency };
        if (sortBy.length) payload.materialize.sort_by = sortBy;
      }

      const statusEl = document.getElementById("save-status");
      const resultEl = document.getElementById("save-result");
      const errorEl = document.getElementById("save-error");
      if (resultEl) resultEl.classList.add("d-none");
      if (errorEl) errorEl.classList.add("d-none");
      if (statusEl) statusEl.textContent = "Saving…";
      submitBtn.disabled = true;

      try {
        const resp = await apiFetch(API_BASE + "/queries?async=true", {
          method: "POST",
          headers: authHeaders(),
          body: JSON.stringify(payload),
        });
        if (resp.status === 403) {
          clearAdminToken();
          if (errorEl) {
            errorEl.textContent = "Forbidden (403) — wrong admin token? Enter it above and retry.";
            errorEl.classList.remove("d-none");
          }
          if (statusEl) statusEl.textContent = "";
          return;
        }
        if (resp.status === 409) {
          const body = await resp.json().catch(function () { return {}; });
          if (nameEl) {
            nameEl.classList.add("is-invalid");
            const fb = document.getElementById("save-name-error");
            if (fb) fb.textContent = (body && body.error) ? body.error : "Name conflict.";
          }
          if (statusEl) statusEl.textContent = "";
          return;
        }
        if (!resp.ok) {
          const text = await resp.text();
          if (errorEl) { errorEl.textContent = "Error " + resp.status + ": " + text; errorEl.classList.remove("d-none"); }
          if (statusEl) statusEl.textContent = "";
          return;
        }
        const data = await resp.json();
        if (statusEl) statusEl.textContent = "";
        if (resultEl) {
          resultEl.classList.remove("d-none");
          const nameSpan = document.getElementById("save-result-name");
          const depsSpan = document.getElementById("save-result-deps");
          const managedFileWrap = document.getElementById("save-result-managed-file");
          const buildingSpan = document.getElementById("save-result-building");
          if (nameSpan) nameSpan.textContent = data.name || name;
          if (depsSpan) depsSpan.textContent = (data.depends_on || []).join(", ") || "none";
          if (managedFileWrap) {
            const managedFile = data.managed_file || "";
            managedFileWrap.classList.toggle("d-none", !managedFile);
            const pathSpan = managedFileWrap.querySelector(".mono");
            if (pathSpan) pathSpan.textContent = managedFile;
          }
          if (buildingSpan) {
            if (data.state === "building") {
              buildingSpan.classList.remove("d-none");
              pollUntilPublished(name, buildingSpan);
            } else {
              buildingSpan.classList.add("d-none");
            }
          }
        }
      } catch (e) {
        if (errorEl) { errorEl.textContent = "Error: " + e.message; errorEl.classList.remove("d-none"); }
        if (statusEl) statusEl.textContent = "";
      } finally {
        submitBtn.disabled = false;
      }
    });
  }

  // Poll /status until terminal — delegates to pollUntilTerminal (defined below).
  // Kept as a thin adapter so the "Save as dataset" call site is unchanged.
  function pollUntilPublished(name, el) {
    pollUntilTerminal(name, function (entry) {
      if (el) el.textContent = entry ? ("State: " + (entry.state || "?") + ".") : "(timed out)";
      // After the dataset reaches a terminal state, insert/update its row
      // in the discovery list so the user can navigate to it without reloading.
      insertDatasetListRow(name, entry);
    });
  }

  // -----------------------------------------------------------------------
  // Insert (or update) a dataset list row after a successful save.
  //
  // Row markup mirrors the Askama template in
  //   crates/core/templates/explorer/index.html  (the {% for d in datasets %}
  //   block).  Keep the two in sync: data-* attributes, badge classes, and
  //   dp-meta format must match so applyStatusToListItem works on both.
  // -----------------------------------------------------------------------
  function insertDatasetListRow(name, entry) {
    const list = document.querySelector(".dataset-list");
    if (!list) return;

    // If a row already exists (e.g. from a previous save), just update it.
    const existing = list.querySelector(
      '.list-group-item[data-name="' + CSS.escape(name) + '"]'
    );
    if (existing) {
      if (entry) applyStatusToListItem(existing, entry);
      return;
    }

    const state  = entry ? (entry.state  || "pending").toLowerCase() : "building";
    const rows   = entry ? (entry.rows   || 0)                       : 0;
    const cols   = entry ? (entry.columns || 0)                      : 0;
    const kind   = entry ? (entry.kind   || "query")                 : "query";

    // Build the badge HTML — same logic as the template's state conditional.
    let badgeHtml;
    if (state === "published") {
      badgeHtml = '<span class="badge text-bg-secondary flex-shrink-0 dp-rows-badge">' + rows + ' rows</span>';
    } else if (state === "building") {
      badgeHtml = '<span class="badge text-bg-warning flex-shrink-0 dp-rows-badge">' +
        '<span class="spinner-border spinner-border-sm" style="width:.6em;height:.6em;border-width:.15em"></span>' +
        ' building</span>';
    } else if (state === "failed") {
      badgeHtml = '<span class="badge text-bg-danger flex-shrink-0 dp-rows-badge">failed</span>';
    } else {
      badgeHtml = '<span class="badge text-bg-light text-dark border flex-shrink-0 dp-rows-badge">pending</span>';
    }

    let metaHtml;
    if (state === "published") {
      metaHtml = kind + " &middot; " + cols + " cols";
    } else if (state === "failed") {
      const err = (entry && entry.last_error) ? entry.last_error.replace(/"/g, "&quot;") : "";
      metaHtml = kind + ' &middot; <span class="text-danger" title="' + err + '">error</span>';
    } else {
      metaHtml = kind + " &middot; loading\u2026";
    }

    const el = document.createElement("a");
    el.className = "list-group-item list-group-item-action";
    el.setAttribute("href", "#");
    el.setAttribute("role", "button");
    el.dataset.name    = name;
    el.dataset.rows    = rows;
    el.dataset.cols    = cols;
    el.dataset.state   = state;
    el.dataset.managed = "true";  // saved via POST /queries → always managed
    // htmx attributes — processed by htmx.process() below.
    el.setAttribute("hx-get",     (CFG.explorerBase || "/explore") + "/datasets/" + encodeURIComponent(name));
    el.setAttribute("hx-target",  "#dataset-detail");
    el.setAttribute("hx-swap",    "innerHTML");
    el.setAttribute("hx-trigger", "click");
    el.setAttribute("onclick",
      "document.querySelectorAll('.dataset-list .list-group-item').forEach(e=>e.classList.remove('active'));" +
      "this.classList.add('active');"
    );
    el.innerHTML =
      '<div class="d-flex justify-content-between align-items-center gap-2">' +
      '  <strong class="text-truncate" style="min-width:0" title="' + name + '">' + name + '</strong>' +
      '  ' + badgeHtml +
      '</div>' +
      '<small class="text-secondary dp-meta">' + metaHtml + '</small>';

    list.appendChild(el);

    // Let htmx process the new element so hx-get fires on click.
    if (typeof htmx !== "undefined") htmx.process(el);
  }

  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // Startup-state polling (COMMIT 2)
  //
  // When datasets are pending or building on page load, poll the listing
  // endpoint at increasing intervals and update each list-item row in place.
  // Polling stops when every dataset reaches a terminal state (published /
  // failed), or when a user navigates away.
  // -----------------------------------------------------------------------

  /**
   * Return the Bootstrap badge class + label for a lifecycle state string.
   */
  function stateBadge(state, lastError) {
    switch (state) {
      case "published": return null;   // rendered as "N rows" — no override badge
      case "building":  return { cls: "text-bg-warning", label: "building", spinner: true };
      case "failed":    return { cls: "text-bg-danger",  label: "failed",   title: lastError || "" };
      default:          return { cls: "text-bg-light text-dark border", label: "pending" };
    }
  }

  /**
   * Update a single list-group-item from a DatasetStatusEntry.
   * `item` is the <a> element; `entry` is the JSON entry from the listing.
   */
  function applyStatusToListItem(item, entry) {
    if (!item) return;
    const state    = (entry.state || "pending").toLowerCase();
    const rows     = entry.rows  || 0;
    const cols     = entry.columns || 0;
    const kind     = item.dataset.kind || entry.kind || "";
    const lastError = entry.last_error || "";

    // Update data attributes used by the sort controls.
    item.dataset.state = state;
    if (state === "published") {
      item.dataset.rows = rows;
      item.dataset.cols = cols;
    }

    // Rows badge.
    const badge = item.querySelector(".dp-rows-badge");
    if (badge) {
      badge.className = "badge flex-shrink-0 dp-rows-badge";
      if (state === "published") {
        badge.className += " text-bg-secondary";
        badge.textContent = rows + " rows";
        badge.removeAttribute("title");
      } else if (state === "building") {
        badge.className += " text-bg-warning";
        badge.innerHTML = `<span class="spinner-border spinner-border-sm" style="width:.6em;height:.6em;border-width:.15em"></span> building`;
      } else if (state === "failed") {
        badge.className += " text-bg-danger";
        badge.textContent = "failed";
        if (lastError) badge.title = lastError;
      } else {
        badge.className += " text-bg-light text-dark border";
        badge.textContent = "pending";
      }
    }

    // Meta line.
    const meta = item.querySelector(".dp-meta");
    if (meta) {
      if (state === "published") {
        meta.innerHTML = kind + " &middot; " + cols + " cols";
      } else if (state === "building" || state === "pending") {
        meta.innerHTML = kind + " &middot; loading&hellip;";
      } else if (state === "failed") {
        const errTitle = lastError ? ` title="${lastError.replace(/"/g, "&quot;")}"` : "";
        meta.innerHTML = `${kind} &middot; <span class="text-danger"${errTitle}>error</span>`;
      }
    }
  }

  /**
   * Start the startup-state polling loop.
   *
   * Polls GET {API_BASE}/datasets at an interval that backs off from 2 s to
   * 8 s.  Stops when no dataset is pending or building, or after 120 ticks.
   */
  function initStartupStatePoll() {
    // Only bother if any list item has a non-published initial state.
    const list = document.querySelector(".dataset-list");
    if (!list) return;
    const allItems = Array.from(list.querySelectorAll(".list-group-item[data-state]"));
    const needsPoll = allItems.some(function (el) {
      return el.dataset.state === "pending" || el.dataset.state === "building";
    });
    if (!needsPoll) return;

    let interval  = 2000;
    let ticks     = 0;
    const MAX     = 120;

    function tick() {
      if (ticks++ >= MAX) return;
      fetch(API_BASE + "/datasets")
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (data) {
          if (!data) { schedule(); return; }
          // data may be { datasets: [...] } or just [...]
          const entries = Array.isArray(data) ? data
            : (Array.isArray(data.datasets) ? data.datasets : []);

          let anyActive = false;
          entries.forEach(function (entry) {
            const state = (entry.state || "pending").toLowerCase();
            const el = list.querySelector(
              `.list-group-item[data-name="${CSS.escape(entry.name)}"]`
            );
            if (el) applyStatusToListItem(el, entry);
            if (state === "pending" || state === "building") anyActive = true;
          });

          if (!anyActive) return;   // all terminal — stop polling
          schedule();
        })
        .catch(schedule);
    }

    function schedule() {
      // Back off up to 8 s.
      interval = Math.min(interval * 1.4, 8000);
      setTimeout(tick, interval);
    }

    // Fire first tick soon after page load.
    setTimeout(tick, 1500);
  }

  // -----------------------------------------------------------------------
  // Reload completion feedback (COMMIT 3)
  // -----------------------------------------------------------------------

  /**
   * After dpReloadDataset triggers a reload, poll /status until terminal,
   * then update the list-item row and dataset-detail state badge.
   */
  function pollUntilTerminal(name, onDone) {
    let attempts = 0;
    const max = 120;
    let interval = 1500;
    function tick() {
      if (attempts++ > max) { if (onDone) onDone(null); return; }
      fetch(API_BASE + "/datasets/" + encodeURIComponent(name) + "/status")
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (data) {
          if (!data) { schedule(); return; }
          const state = (data.state || "").toLowerCase();
          // Update the list-item row.
          const list  = document.querySelector(".dataset-list");
          const el    = list && list.querySelector(
            `.list-group-item[data-name="${CSS.escape(name)}"]`
          );
          if (el) applyStatusToListItem(el, data);
          // Update the detail-pane state badge (may be from a previous htmx load).
          const detailBadge = document.getElementById("state-badge-" + name);
          if (detailBadge) {
            if (state === "published") {
              detailBadge.className = "badge text-bg-success ms-1";
              detailBadge.textContent = "reloaded";
              setTimeout(function () {
                detailBadge.className = "";
                detailBadge.textContent = "";
              }, 4000);
            } else if (state === "failed") {
              detailBadge.className = "badge text-bg-danger ms-1";
              detailBadge.textContent = "failed";
              if (data.last_error) detailBadge.title = data.last_error;
            }
          }
          if (state === "building" || state === "pending") {
            schedule();
          } else {
            if (onDone) onDone(data);
          }
        })
        .catch(schedule);
    }
    function schedule() {
      interval = Math.min(interval * 1.3, 5000);
      setTimeout(tick, interval);
    }
    tick();
  }
  // -----------------------------------------------------------------------
  // Init
  // -----------------------------------------------------------------------
  document.addEventListener("DOMContentLoaded", function () {
    initReloadAll();
    initSaveAsDataset();
    initStartupStatePoll();
    // Initialize Bootstrap tooltips for truncated dataset name labels.
    // This runs in {% block scripts %} which loads after bootstrap.bundle.min.js
    // (in the base template's <body> tail), so `bootstrap` is defined here.
    if (typeof bootstrap !== "undefined" && bootstrap.Tooltip) {
      document.querySelectorAll("[data-bs-toggle=\"tooltip\"]").forEach(function (el) {
        new bootstrap.Tooltip(el);
      });
    }
  });
  // fires for it). The inline onclick attrs call window.dpReloadDataset /
  // window.dpDeleteDataset which are already set above.
})();
