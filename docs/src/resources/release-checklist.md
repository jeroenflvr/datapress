# Release checklist — 0.7.0 and later

This document records the manual verification steps required before tagging
a release. Complete every item and tick it off before pushing the release tag.

---

## Automated gates (CI must be green)

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --workspace -D warnings` passes
- [ ] Full test suite passes on both backends (`cargo test --workspace`)
- [ ] Full-feature build compiles: `cargo build -p datapress-datafusion --features pgwire,docs,swagger,auth,metrics,explorer`
- [ ] Docs build clean: `task docs:build` (or `mkdocs build --strict`)

---

## Version bump checklist

- [ ] `[workspace.package] version` in root `Cargo.toml` updated
- [ ] Workspace dependency versions (`datapress-core`, `datapress-duckdb`, `datapress-datafusion`, `datapress-client`) updated to match
- [ ] `extra.datapress_version` in `docs/mkdocs.yml` updated
- [ ] `CHANGELOG.md` has an entry for the new version with a date
- [ ] `[Unreleased]` section cleared (no lingering unreleased items)

---

## BREAKING changes in 0.7.0

Two items require explicit callout in release notes:

1. **Legacy `/api/...` alias removed.** All routes now live under `/api/v1/...` only.
   Clients using the old paths must update. A `404` with the standard error body
   is returned for all former legacy paths.

2. **Non-blocking startup.** The server binds and serves immediately. A request
   sent right after process start may receive `503 Service Unavailable` while
   `eager` datasets are still building. Orchestrators and smoke tests **must
   gate on `/readyz`** rather than a single query call.

---

## Manual explorer verification flow

Walk through this flow with a live server built with `--features explorer`:

- [ ] **Discovery**: open `/explore` — dataset list appears, state badges visible.
- [ ] **Save as dataset (temp)**: run a SQL query in the API Query tab → click
      "Save as dataset" → select `kind = temp`, name it, submit → dataset appears
      in the list as `building`, transitions to `published`.
- [ ] **Query a saved dataset**: select the saved dataset from the dropdown and
      run a query — results appear.
- [ ] **Reload button**: click the reload button on the saved dataset's row →
      badge transitions `building → published`; button is disabled while building.
- [ ] **Reload all**: click the "Reload all" button → confirm the dialog showing
      the number of datasets affected → `enqueued` / `skipped` summary appears.
- [ ] **Delete managed dataset**: click delete on the saved dataset → confirm →
      dataset disappears from the list.
- [ ] **409 on delete with dependent**: create dataset A and dataset B that
      depends on A → try to delete A → inline `409` error listing B appears;
      A survives.
- [ ] **Auth missing**: with no `ADMIN_TOKEN` and no OIDC configured → save/delete
      actions are hidden entirely.

---

## Smoke tests

Run the `task demo:materialized` script (or equivalent) and confirm:

- [ ] `/healthz` responds within 1 s of process start while a large dataset is still loading.
- [ ] `/readyz` returns `503` immediately after boot, transitions to `200` on publish.
- [ ] A `query` dataset with `interval = "1m"` refreshes automatically.
- [ ] `X-Dataset-Refreshed-At` header changes after a refresh.
- [ ] A `lazy` dataset (`residency = "lazy"`) is served from storage and the
      `/status` endpoint shows `residency = "lazy"` and a non-null `storage_bytes`.
- [ ] `POST /api/v1/datasets/reload-all` returns `202` with `enqueued` and
      `skipped` lists and datasets rebuild in topological order.
- [ ] `POST /api/v1/queries` + `DELETE /api/v1/queries/{name}` lifecycle works.
- [ ] `GET /api/v1/datasets` lists all datasets with correct `state` and `kind`.

---

## Documentation review

- [ ] [Materialized datasets](../configuration/materialized.md) page renders correctly.
- [ ] [Saved queries](../operations/saved-queries.md) page renders correctly.
- [ ] CONFIG.md new sections readable and accurate.
- [ ] README API versioning section says 0.7.0, not an earlier version.
- [ ] CHANGELOG 0.7.0 section is complete and dated.
- [ ] `datapress init` template includes the commented-out `query` dataset example.

---

## Python wheel smoke (optional but recommended)

- [ ] `task py:develop` builds the wheel without errors.
- [ ] `python -c "from datap_rs.datapress import DatasetConfig; print(DatasetConfig.__doc__)"` runs.

---

## After tagging

- [ ] Push the tag: `git push origin v0.7.0`
- [ ] GitHub release created with CHANGELOG entries copy-pasted.
- [ ] Docker image tagged and pushed.
- [ ] Homebrew formula updated (if applicable).
- [ ] PyPI wheels uploaded (if applicable).
