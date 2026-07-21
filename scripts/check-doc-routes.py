#!/usr/bin/env python3
"""Route-diff check: every HTTP route registered in the code must be
documented in docs/src/reference/endpoints.md.

Sources of truth:
  * crates/core/src/handlers/v1.rs   — the `ROUTES` const (versioned API).
  * crates/core/src/handlers/mod.rs  — probe handlers via `#[get("...")]`.

The check is a substring match: each code route's path suffix must appear
somewhere in endpoints.md (versioned routes are matched as `/api/v1<suffix>`).
Exits non-zero and prints the offenders if anything is undocumented, so a
new route can't ship without a matching line in the reference.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
V1 = ROOT / "crates/core/src/handlers/v1.rs"
MOD = ROOT / "crates/core/src/handlers/mod.rs"
DOC = ROOT / "docs/src/reference/endpoints.md"

# ("METHOD", "/suffix") entries from the ROUTES const in v1.rs.
ROUTES_RE = re.compile(r'\(\s*"([A-Z]+)"\s*,\s*"([^"]+)"\s*\)')
# Probe path from attribute macros like #[get("/healthz")].
PROBE_RE = re.compile(r'#\[(?:get|post|put|delete|head)\("([^"]+)"\)\]')


def v1_routes() -> list[tuple[str, str]]:
    text = V1.read_text(encoding="utf-8")
    # Isolate the ROUTES const body so unrelated tuples aren't picked up.
    m = re.search(r"pub const ROUTES:[^=]*=\s*&\[(.*?)\];", text, re.S)
    if not m:
        sys.exit("error: could not locate ROUTES const in v1.rs")
    return [(meth, f"/api/v1{path}") for meth, path in ROUTES_RE.findall(m.group(1))]


def probe_routes() -> list[tuple[str, str]]:
    text = MOD.read_text(encoding="utf-8")
    return [("GET", path) for path in PROBE_RE.findall(text)]


def main() -> int:
    doc = DOC.read_text(encoding="utf-8")
    missing = [
        (meth, path)
        for meth, path in v1_routes() + probe_routes()
        if path not in doc
    ]
    if missing:
        print("Undocumented routes (missing from docs/src/reference/endpoints.md):")
        for meth, path in missing:
            print(f"  {meth:6} {path}")
        return 1
    print("All code routes are documented in endpoints.md.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
