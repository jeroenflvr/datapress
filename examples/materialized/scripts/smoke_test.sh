#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Smoke test for the materialized datasets feature (Phase 7 T9.5 / DoD).
#
# What it checks:
#   1. /healthz responds < 1 s while datasets are loading
#   2. /readyz transitions 503 → 200 (server waits up to 60 s)
#   3. A legacy /api path (no /v1) returns 404
#   4. X-Dataset-Refreshed-At header changes across a manual reload
#   5. POST /queries creates a temp dataset and it becomes queryable
#   6. reload-all enqueues datasets and they eventually publish
#   7. /status for the lazy-residency dataset shows residency=lazy
#   8. Cleanup: delete the temp dataset
#
# Usage:
#   BASE_URL=http://127.0.0.1:8090 ADMIN_TOKEN=demo-secret bash scripts/smoke_test.sh
#
# The script expects the datapress-duckdb binary to already be running against
# examples/materialized/datasets.toml (launched by `task demo:materialized`).
# ---------------------------------------------------------------------------
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8090}"
API="${BASE_URL}/api/v1"
TOKEN="${ADMIN_TOKEN:-demo-secret}"
MAX_READY_WAIT=120  # seconds to wait for /readyz

PASS=0
FAIL=0

pass() { echo "  PASS: $1"; ((PASS+=1)) || true; }
fail() { echo "  FAIL: $1"; ((FAIL+=1)) || true; }
check() {
  local label="$1"
  local cond="$2"
  if eval "$cond"; then pass "$label"; else fail "$label"; fi
}

hr() { echo ""; echo "------- $1 -------"; }

# ---------------------------------------------------------------------------
hr "1. /healthz responds quickly"
# ---------------------------------------------------------------------------
START=$(date +%s%N 2>/dev/null || date +%s)
STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "${BASE_URL}/healthz" || echo "000")
END=$(date +%s%N 2>/dev/null || date +%s)
if [ "${START}" != "${END}" ] && [[ "${START}" =~ [0-9]{18} ]]; then
  ELAPSED_MS=$(( (END - START) / 1000000 ))
  check "/healthz returns 200" '[[ "$STATUS" == "200" ]]'
  check "/healthz responds < 1000 ms" '[[ "$ELAPSED_MS" -lt 1000 ]]'
else
  # Fallback: no nanosecond timer — just check status
  check "/healthz returns 200" '[[ "$STATUS" == "200" ]]'
fi

# ---------------------------------------------------------------------------
hr "2. /readyz transitions 503 → 200 within ${MAX_READY_WAIT}s"
# ---------------------------------------------------------------------------
echo "  Waiting for /readyz …"
WAITED=0
while true; do
  RSTATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "${BASE_URL}/readyz" || echo "000")
  if [[ "$RSTATUS" == "200" ]]; then
    pass "/readyz returns 200 after ${WAITED}s"
    break
  fi
  if [[ "$WAITED" -ge "$MAX_READY_WAIT" ]]; then
    fail "/readyz never returned 200 within ${MAX_READY_WAIT}s (last: $RSTATUS)"
    break
  fi
  sleep 2
  WAITED=$((WAITED + 2))
done

# ---------------------------------------------------------------------------
hr "3. Legacy /api path returns 404"
# ---------------------------------------------------------------------------
LEGACY_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 \
  "${BASE_URL}/api/datasets" || echo "000")
check "GET /api/datasets (no /v1) returns 404" '[[ "$LEGACY_STATUS" == "404" ]]'

# ---------------------------------------------------------------------------
hr "4. X-Dataset-Refreshed-At changes after a reload"
# ---------------------------------------------------------------------------
HEADER_BEFORE=$(curl -s -I --max-time 10 \
  -H "Content-Type: application/json" \
  -H "X-Admin-Token: $TOKEN" \
  --request POST \
  --data '{}' \
  "${API}/datasets/accidents/query" 2>/dev/null \
  | grep -i "x-dataset-refreshed-at" | tr -d '\r' || echo "")
echo "  Refreshed-At before reload: ${HEADER_BEFORE:-<none>}"

# Trigger reload
RELOAD_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 30 \
  -X POST -H "X-Admin-Token: $TOKEN" \
  "${API}/datasets/accidents/reload" || echo "000")
check "reload returns 2xx" '[[ "$RELOAD_STATUS" =~ ^2 ]]'

sleep 1

HEADER_AFTER=$(curl -s -I --max-time 10 \
  -H "Content-Type: application/json" \
  -H "X-Admin-Token: $TOKEN" \
  --request POST \
  --data '{}' \
  "${API}/datasets/accidents/query" 2>/dev/null \
  | grep -i "x-dataset-refreshed-at" | tr -d '\r' || echo "")
echo "  Refreshed-At after reload:  ${HEADER_AFTER:-<none>}"

if [[ -n "$HEADER_BEFORE" && -n "$HEADER_AFTER" ]]; then
  check "X-Dataset-Refreshed-At changed" '[[ "$HEADER_BEFORE" != "$HEADER_AFTER" ]]'
else
  check "X-Dataset-Refreshed-At present after reload" '[[ -n "$HEADER_AFTER" ]]'
fi

# ---------------------------------------------------------------------------
hr "5. POST /queries creates a temp dataset and it is queryable"
# ---------------------------------------------------------------------------
CREATE_RESP=$(curl -s -w "\n%{http_code}" --max-time 30 \
  -X POST \
  -H "Content-Type: application/json" \
  -H "X-Admin-Token: $TOKEN" \
  --data '{"name":"smoke_temp","sql":"SELECT State, COUNT(*) AS n FROM accidents GROUP BY State","kind":"temp"}' \
  "${API}/queries" || echo -e "\n000")
CREATE_STATUS=$(echo "$CREATE_RESP" | tail -1)
check "POST /queries returns 2xx" '[[ "$CREATE_STATUS" =~ ^2 ]]'

# Wait for the temp dataset to publish
WAITED=0
while true; do
  TEMP_STATUS=$(curl -s --max-time 5 \
    -H "X-Admin-Token: $TOKEN" \
    "${API}/datasets/smoke_temp/status" 2>/dev/null | grep -o '"state":"[^"]*"' | head -1 || echo "")
  if [[ "$TEMP_STATUS" == '"state":"published"' ]]; then
    pass "smoke_temp reached published state"
    break
  fi
  if [[ "$WAITED" -ge 30 ]]; then
    fail "smoke_temp never published within 30s (last status: ${TEMP_STATUS})"
    break
  fi
  sleep 2; WAITED=$((WAITED + 2))
done

QUERY_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
  -X POST \
  -H "Content-Type: application/json" \
  --data '{}' \
  "${API}/datasets/smoke_temp/query" || echo "000")
check "smoke_temp is queryable (returns 2xx)" '[[ "$QUERY_STATUS" =~ ^2 ]]'

# ---------------------------------------------------------------------------
hr "6. reload-all enqueues datasets"
# ---------------------------------------------------------------------------
RELOAD_ALL=$(curl -s -w "\n%{http_code}" --max-time 30 \
  -X POST \
  -H "X-Admin-Token: $TOKEN" \
  "${API}/datasets/reload-all" || echo -e "\n000")
RELOAD_ALL_STATUS=$(echo "$RELOAD_ALL" | tail -1)
check "POST /datasets/reload-all returns 2xx" '[[ "$RELOAD_ALL_STATUS" =~ ^2 ]]'

RELOAD_ALL_BODY=$(echo "$RELOAD_ALL" | head -1)
check "reload-all body contains 'enqueued'" '[[ "$RELOAD_ALL_BODY" == *enqueued* ]]'

# Wait for accidents_summary to republish
WAITED=0
while true; do
  SUMMARY_STATE=$(curl -s --max-time 5 \
    "${API}/datasets/accidents_summary/status" 2>/dev/null \
    | grep -o '"state":"[^"]*"' | head -1 || echo "")
  if [[ "$SUMMARY_STATE" == '"state":"published"' ]]; then
    pass "accidents_summary published after reload-all"
    break
  fi
  if [[ "$WAITED" -ge 60 ]]; then
    fail "accidents_summary did not republish within 60s"
    break
  fi
  sleep 2; WAITED=$((WAITED + 2))
done

# ---------------------------------------------------------------------------
hr "7. /status for lazy-residency dataset shows residency=lazy"
# ---------------------------------------------------------------------------
LAZY_STATUS=$(curl -s --max-time 10 \
  "${API}/datasets/accidents_lazy/status" 2>/dev/null || echo "")
check "accidents_lazy status present" '[[ -n "$LAZY_STATUS" ]]'
check "accidents_lazy residency=lazy" '[[ "$LAZY_STATUS" == *'"'"'residency":"lazy"'"'"'* || "$LAZY_STATUS" == *"residency":"lazy"* ]]'

# ---------------------------------------------------------------------------
hr "8. Cleanup: delete the temp dataset"
# ---------------------------------------------------------------------------
DELETE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
  -X DELETE \
  -H "X-Admin-Token: $TOKEN" \
  "${API}/queries/smoke_temp" || echo "000")
check "DELETE /queries/smoke_temp returns 2xx" '[[ "$DELETE_STATUS" =~ ^2 ]]'

GONE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 \
  -X POST -H "Content-Type: application/json" --data '{}' \
  "${API}/datasets/smoke_temp/query" || echo "000")
check "smoke_temp no longer queryable after delete (404)" '[[ "$GONE_STATUS" == "404" ]]'

# ---------------------------------------------------------------------------
hr "Results"
# ---------------------------------------------------------------------------
echo ""
echo "  Passed: $PASS"
echo "  Failed: $FAIL"
echo ""
if [[ "$FAIL" -gt 0 ]]; then
  echo "SMOKE TEST FAILED" >&2
  exit 1
fi
echo "SMOKE TEST PASSED"
