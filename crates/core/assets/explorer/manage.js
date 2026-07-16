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
    const btn = document.getElementById("reload-btn-" + name);
    const badge = document.getElementById("state-badge-" + name);
    if (btn) {
      btn.disabled = true;
      btn.innerHTML = '<span class="spinner-border spinner-border-sm"></span>';
    }
    if (badge) {
      badge.className = "badge text-bg-warning ms-1";
      badge.textContent = "building";
    }
    try {
      const resp = await apiFetch(apiBase + "/datasets/" + encodeURIComponent(name) + "/reload", {
        method: "POST",
        headers: authHeaders(),
      });
      if (resp.status === 403) {
        if (!ensureToken("save-admin-token")) return;
        // Re-prompt and retry once.
        const resp2 = await apiFetch(apiBase + "/datasets/" + encodeURIComponent(name) + "/reload", {
          method: "POST",
          headers: authHeaders(),
        });
        if (!resp2.ok) {
          alert("Reload failed: " + resp2.status);
          return;
        }
      } else if (!resp.ok) {
        const text = await resp.text();
        alert("Reload failed: " + resp.status + "\n" + text);
        return;
      }
      if (badge) {
        badge.className = "badge text-bg-success ms-1";
        badge.textContent = "reloaded";
        setTimeout(function () {
          badge.className = "";
          badge.textContent = "";
        }, 4000);
      }
    } catch (e) {
      alert("Reload error: " + e.message);
    } finally {
      if (btn) {
        btn.disabled = false;
        btn.innerHTML = '<i class="bi bi-arrow-clockwise"></i> Reload';
      }
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
      // Reload the page to reflect the removal.
      window.location.reload();
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
      btn.innerHTML = '<span class="spinner-border spinner-border-sm"></span> Reloading…';

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
        const enqueued = (body.enqueued || []).join(", ") || "none";
        const skipped = (body.skipped || []).join(", ") || "none";
        // Show a transient summary.
        const existing = document.getElementById("reload-all-summary");
        if (existing) existing.remove();
        const summary = document.createElement("div");
        summary.id = "reload-all-summary";
        summary.className = "alert alert-info alert-dismissible mt-2 small";
        summary.innerHTML = "<strong>Reload-all:</strong> enqueued: " + enqueued +
          " | skipped: " + skipped +
          '<button type="button" class="btn-close" data-bs-dismiss="alert" aria-label="Close"></button>';
        btn.parentNode.insertAdjacentElement("afterend", summary);
        setTimeout(function () { if (summary.parentNode) summary.remove(); }, 10000);
      } catch (e) {
        alert("Reload-all error: " + e.message);
      } finally {
        btn.disabled = false;
        btn.innerHTML = '<i class="bi bi-arrow-repeat"></i> Reload all';
      }
    });
  }

  // -----------------------------------------------------------------------
  // R8.7 — Save as dataset form
  // -----------------------------------------------------------------------
  function initSaveAsDataset() {
    if (!QUERIES_ENABLED) return;

    // Show the save-as-dataset toggle button in the API Query tab.
    const wrap = document.getElementById("save-as-dataset-wrap");
    if (wrap) wrap.classList.remove("d-none");

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
          const buildingSpan = document.getElementById("save-result-building");
          if (nameSpan) nameSpan.textContent = data.name || name;
          if (depsSpan) depsSpan.textContent = (data.depends_on || []).join(", ") || "none";
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

  // Poll /status until state != building, then update the badge.
  function pollUntilPublished(name, el) {
    let attempts = 0;
    const max = 60;   // 60 × 2s = 2 min max
    function tick() {
      if (attempts++ > max) { if (el) el.textContent = "(timed out polling)"; return; }
      fetch(API_BASE + "/datasets/" + encodeURIComponent(name) + "/status")
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (data) {
          if (!data) return;
          if (data.state === "building") {
            setTimeout(tick, 2000);
          } else {
            if (el) el.textContent = "State: " + data.state + ".";
          }
        })
        .catch(function () { setTimeout(tick, 2000); });
    }
    setTimeout(tick, 2000);
  }

  // -----------------------------------------------------------------------
  // Init
  // -----------------------------------------------------------------------
  document.addEventListener("DOMContentLoaded", function () {
    initReloadAll();
    initSaveAsDataset();
  });

  // Also expose for dataset.html which is loaded via htmx (no DOMContentLoaded
  // fires for it). The inline onclick attrs call window.dpReloadDataset /
  // window.dpDeleteDataset which are already set above.
})();
