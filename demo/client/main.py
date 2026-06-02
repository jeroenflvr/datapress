"""DuckDB Shell client — a small FastAPI + htmx + Jinja2 + Bootstrap app.

Serves an in-browser DuckDB-WASM shell and an htmx-powered panel that
connects to a DataPress / quack server to list its datasets.

Run with:

    uv run uvicorn main:app --reload
"""

from __future__ import annotations

import os
from pathlib import Path

import httpx
from fastapi import FastAPI, Form, Request
from fastapi.responses import HTMLResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

BASE_DIR = Path(__file__).resolve().parent
STATIC_DIR = BASE_DIR / "static"
TEMPLATES_DIR = BASE_DIR / "templates"

# DuckDB-WASM build to load in the browser. See the original notes: only the
# 1.5.3 engine ships quack's wasm binary, published under the "next" dist-tag.
DUCKDB_VERSION = os.getenv("DUCKDB_WASM_VERSION", "next")

# Default DataPress server the dataset panel points at. Overridable per-request
# via the form, and at startup via the DATAPRESS_URL env var.
DEFAULT_SERVER_URL = os.getenv("DATAPRESS_URL", "http://localhost:8080")

app = FastAPI(title="DuckDB Shell Client")
STATIC_DIR.mkdir(exist_ok=True)
app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")
templates = Jinja2Templates(directory=str(TEMPLATES_DIR))


@app.get("/", response_class=HTMLResponse)
async def index(request: Request) -> HTMLResponse:
    """Render the shell page."""
    return templates.TemplateResponse(
        request,
        "index.html",
        {
            "duckdb_version": DUCKDB_VERSION,
            "run_quack": True,
            "default_server_url": DEFAULT_SERVER_URL,
        },
    )


@app.post("/datasets", response_class=HTMLResponse)
async def datasets(request: Request, server_url: str = Form(...)) -> HTMLResponse:
    """htmx endpoint: fetch and render a DataPress server's datasets."""
    base = server_url.rstrip("/")
    context: dict[str, object] = {"server_url": server_url}
    try:
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(f"{base}/api/v1/datasets")
            resp.raise_for_status()
            payload = resp.json()
    except httpx.HTTPStatusError as exc:
        context["error"] = f"Server returned {exc.response.status_code}."
    except httpx.HTTPError as exc:
        context["error"] = f"Could not reach {base}: {exc}"
    except ValueError:
        context["error"] = "Server response was not valid JSON."
    else:
        # Accept either a bare list or a `{ "datasets": [...] }` envelope.
        items = payload.get("datasets", payload) if isinstance(payload, dict) else payload
        context["datasets"] = items if isinstance(items, list) else []

    return templates.TemplateResponse(request, "partials/datasets.html", context)


def main() -> None:
    import uvicorn

    uvicorn.run("main:app", host="0.0.0.0", port=8888, reload=True)


if __name__ == "__main__":
    main()
