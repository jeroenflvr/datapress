# DuckDB Shell client

A small **FastAPI + htmx + Jinja2 + Bootstrap** app that serves an in-browser
[DuckDB-WASM](https://github.com/duckdb/duckdb-wasm) shell and an htmx-powered
panel for listing datasets on a DataPress / quack server.

## Run

```sh
uv run uvicorn main:app --reload
```

Then open http://127.0.0.1:8000.

## Layout

| Path                              | Purpose                                            |
| --------------------------------- | -------------------------------------------------- |
| `main.py`                         | FastAPI app (routes, htmx endpoints)               |
| `templates/index.html`            | Page shell (Bootstrap, htmx, Jinja2)               |
| `templates/partials/datasets.html`| htmx fragment for the dataset list                 |
| `static/shell.js`                 | DuckDB-WASM boot logic                              |
| `static/app.css`                  | Layout tweaks on top of Bootstrap                  |

## Configuration

Set via environment variables:

| Variable              | Default                  | Notes                                  |
| --------------------- | ------------------------ | -------------------------------------- |
| `DUCKDB_WASM_VERSION` | `next`                   | duckdb-wasm dist-tag/version to load.  |
| `DATAPRESS_URL`       | `http://localhost:8080`  | Default server URL in the panel form.  |
