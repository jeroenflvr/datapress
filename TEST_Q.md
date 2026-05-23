# Test queries

A catalogue of `curl` smoke tests and `oha` benchmarks against either backend
(`fast-api-duckdb` or `fast-api-datafusion`) listening on `:8080`.

The dataset is the US Accidents corpus (~7.7M rows, ~45 columns).

---

## Smoke tests (`curl` + `jq`)

These send a single request and pretty-print the result — useful for sanity
checks after restarting a backend.

### S1. Severity ≥ 3 in Texas, projected columns

Tests the common case: one equality predicate (`State = 'TX'`, hits the
equality index on the DataFusion side) combined with a range predicate. Small
projection, small page.

```bash
curl -s -X POST http://localhost:8080/api/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID", "Severity", "City", "State", "Start_Time", "Weather_Condition", "Temperature(F)"],
    "predicates": [
      { "col": "State",    "op": "eq",  "val": "TX" },
      { "col": "Severity", "op": "gte", "val": 3 }
    ],
    "page": 1,
    "page_size": 5
  }' | jq .
```

### S2. `ILIKE` on `Description` + temperature range

Combines a substring search on a large `Utf8` column with a numeric range —
exercises the string kernel and the numeric kernel in a single query.

```bash
curl -s -X POST http://localhost:8080/api/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID", "City", "State", "Start_Time", "Description", "Temperature(F)", "Severity"],
    "predicates": [
      { "col": "Description",    "op": "ilike", "val": "%fog%" },
      { "col": "Temperature(F)", "op": "lt",    "val": 32 }
    ],
    "page": 1,
    "page_size": 5
  }' | jq .
```

### S3. `IN` list — multiple states

Validates that the `IN` operator on a low-cardinality column resolves to a
disjunction over the equality index.

```bash
curl -s -X POST http://localhost:8080/api/accidents/query \
  -H 'Content-Type: application/json' \
  -d '{
    "columns": ["ID", "City", "State", "Severity", "Start_Time", "Weather_Condition"],
    "predicates": [
      { "col": "State",    "op": "in",  "val": ["NY", "NJ", "CT"] },
      { "col": "Severity", "op": "eq",  "val": 4 }
    ],
    "page": 1,
    "page_size": 10
  }' | jq .
```

---

## Light benchmarks (`oha`)

Sustained load at moderate concurrency; everything below should be sub-ms per
request on a warm cache.

### L1. GET — California severity 3

Cheapest possible path: GET handler with two equality filters, page size 5.

```bash
oha -c 4 -n 10000 "http://127.0.0.1:8080/api/accidents?state=CA&severity=3&page=1&page_size=5"
```

### L2. GET — Miami, FL

Two equality filters on string columns. Verifies that the equality index
covers `City` as well as `State`.

```bash
oha -c 4 -n 10000 "http://127.0.0.1:8080/api/accidents?city=Miami&state=FL&page_size=3"
```

### L3. POST — severity ≥ 3 in Texas

POST equivalent of S1 under load. Compare against L1 to measure the cost of
JSON body parsing vs query-string parsing.

```bash
oha -c 4 -n 10000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","Severity","City","State","Start_Time","Weather_Condition","Temperature(F)"],"predicates":[{"col":"State","op":"eq","val":"TX"},{"col":"Severity","op":"gte","val":3}],"page":1,"page_size":5}' \
  http://127.0.0.1:8080/api/accidents/query
```

### L4. POST — fog + below freezing

Mixed `ILIKE` + numeric range under load. Probably the highest per-request
cost in this section.

```bash
oha -c 4 -n 10000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Start_Time","Description","Temperature(F)","Severity"],"predicates":[{"col":"Description","op":"ilike","val":"%fog%"},{"col":"Temperature(F)","op":"lt","val":32}],"page":1,"page_size":5}' \
  http://127.0.0.1:8080/api/accidents/query
```

### L5. POST — `IN` (NY/NJ/CT) severity 4

POST equivalent of S3 under load.

```bash
oha -c 4 -n 10000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Severity","Start_Time","Weather_Condition"],"predicates":[{"col":"State","op":"in","val":["NY","NJ","CT"]},{"col":"Severity","op":"eq","val":4}],"page":1,"page_size":10}' \
  http://127.0.0.1:8080/api/accidents/query
```

---

## Heavy benchmarks — CPU / memory stress

Each `H*` query is engineered to defeat one specific optimisation, so the
backends can be profiled in isolation. Concurrency (`-c`) and request count
(`-n`) are scaled down relative to the light section because per-request cost
is much higher.

### H1. Double `ILIKE` on `Description`, rare match

Two unanchored substring searches over the largest string column. Neither
side can use the equality index, so the executor must scan ~7.7M rows ×
2 substring matchers. Final result set is tiny — this is a pure CPU test of
the string kernel, not a memory test.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","State","Start_Time","Description","Severity"],"predicates":[{"col":"Description","op":"ilike","val":"%black ice%"},{"col":"Description","op":"ilike","val":"%bridge%"}],"page":1,"page_size":20}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H2. Wide numeric range — hot temperature + low visibility

Three range predicates on `Float64` columns with no equality fast-path.
Candidate set is large (warm-weather conditions are common) so the executor
emits many rows before the LIMIT bites. Measures the throughput of the
SIMD numeric comparator + the cost of building a wide candidate batch.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Temperature(F)","Visibility(mi)","Weather_Condition","Start_Time"],"predicates":[{"col":"Temperature(F)","op":"gte","val":80},{"col":"Temperature(F)","op":"lte","val":100},{"col":"Visibility(mi)","op":"lt","val":2}],"page":1,"page_size":50}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H3. Geographic bounding box (US southwest quadrant)

Four range predicates on two `Float64` columns (`Start_Lat`, `Start_Lng`).
There is no spatial index, so this is fully SIMD-bound: every row's lat/lng
is compared four times. A realistic GIS workload.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","Start_Lat","Start_Lng","City","State","Severity","Start_Time"],"predicates":[{"col":"Start_Lat","op":"gte","val":32.0},{"col":"Start_Lat","op":"lte","val":37.0},{"col":"Start_Lng","op":"gte","val":-120.0},{"col":"Start_Lng","op":"lte","val":-110.0}],"page":1,"page_size":100}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H4. Wide projection — all ~45 columns, page_size 500

Selective filter (`State='CA' AND Severity>=3`) keeps the scan cheap, but the
response carries 500 rows × 45 columns ≈ 22.5k cell serialisations per
request. Stresses the JSON encoder and the per-response heap allocations,
not the scan path.

```bash
oha -c 4 -n 1000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","Source","Severity","Start_Time","End_Time","Start_Lat","Start_Lng","End_Lat","End_Lng","Distance(mi)","Description","Street","City","County","State","Zipcode","Country","Timezone","Airport_Code","Weather_Timestamp","Temperature(F)","Wind_Chill(F)","Humidity(%)","Pressure(in)","Visibility(mi)","Wind_Direction","Wind_Speed(mph)","Precipitation(in)","Weather_Condition","Amenity","Bump","Crossing","Give_Way","Junction","No_Exit","Railway","Roundabout","Station","Stop","Traffic_Calming","Traffic_Signal","Turning_Loop","Sunrise_Sunset","Civil_Twilight","Nautical_Twilight","Astronomical_Twilight"],"predicates":[{"col":"State","op":"eq","val":"CA"},{"col":"Severity","op":"gte","val":3}],"page":1,"page_size":500}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H5. Deep pagination — page 5000

`OFFSET 249_950 LIMIT 50` over a filter that matches most rows
(`Severity >= 2`). Both backends must produce and discard ~250k rows before
the first byte of the response is written. Highlights the absence of an
ORDER BY + cursor-based pagination strategy.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Severity","Start_Time"],"predicates":[{"col":"Severity","op":"gte","val":2}],"page":5000,"page_size":50}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H6. Large `IN` list (20 states) + `ILIKE`

A 20-element `IN` list expands to a 20-way equality disjunction; combined
with a substring match on `Description`, this exercises both the index-merge
path and the string kernel simultaneously. Result set is also large, so the
LIMIT only saves a fraction of the work.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Description","Severity","Start_Time"],"predicates":[{"col":"State","op":"in","val":["CA","TX","FL","NY","PA","IL","OH","GA","NC","MI","NJ","VA","WA","AZ","MA","TN","IN","MO","MD","WI"]},{"col":"Description","op":"ilike","val":"%accident%"}],"page":1,"page_size":100}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H7. Boolean flag conjunction + range

Three boolean columns (`Junction`, `Traffic_Signal`, `Crossing`) ANDed
together, then narrowed by `Severity >= 3`. Booleans are stored as bitmaps,
so the predicate cost is dominated by bitmap intersection rather than
SIMD comparison — a different code path from H2/H3.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","City","State","Junction","Traffic_Signal","Crossing","Severity"],"predicates":[{"col":"Junction","op":"eq","val":true},{"col":"Traffic_Signal","op":"eq","val":true},{"col":"Crossing","op":"eq","val":true},{"col":"Severity","op":"gte","val":3}],"page":1,"page_size":50}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H8. Triple `ILIKE` on `Description` — worst-case string CPU

The most punishing per-row workload here: three substring searches on the
same large string column. None of the patterns is anchored, so each row
incurs three full scans of its `Description` value.

```bash
oha -c 4 -n 1000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","State","Description","Start_Time"],"predicates":[{"col":"Description","op":"ilike","val":"%closed%"},{"col":"Description","op":"ilike","val":"%lane%"},{"col":"Description","op":"ilike","val":"%due to%"}],"page":1,"page_size":50}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H9. `IS NOT NULL` + range on a sparse column

`Precipitation(in)` is heavily nullable. This query first filters out
nulls, then applies a range predicate to the survivors, then ANDs with a
range on `Wind_Speed(mph)`. Exercises the null-bitmap fast-path and shows
how each backend chains nullable comparators.

```bash
oha -c 8 -n 2000 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","Precipitation(in)","Weather_Condition","State","Start_Time"],"predicates":[{"col":"Precipitation(in)","op":"is_not_null"},{"col":"Precipitation(in)","op":"gt","val":0.5},{"col":"Wind_Speed(mph)","op":"gte","val":20}],"page":1,"page_size":100}' \
  http://127.0.0.1:8080/api/accidents/query
```

### H10. Full table, no predicates, 18 columns × 1000 rows

No filtering at all — pure projection + serialisation throughput. With
`page_size=1000` and 18 columns, each response is ≥18k cells. Low
concurrency (`-c 2`) avoids saturating the link before the backend.

```bash
oha -c 2 -n 200 -m POST \
  -H "Content-Type: application/json" \
  -d '{"columns":["ID","Source","Severity","Start_Time","End_Time","Start_Lat","Start_Lng","Distance(mi)","Description","City","County","State","Zipcode","Temperature(F)","Humidity(%)","Visibility(mi)","Wind_Speed(mph)","Weather_Condition"],"predicates":[],"page":1,"page_size":1000}' \
  http://127.0.0.1:8080/api/accidents/query
```

---

## Suggested workflow

1. Start one backend: `task run:duckdb` (or `task run:datafusion`).
2. Run the smoke tests `S1`–`S3` to confirm shape of responses.
3. Warm the cache by running `L1` once.
4. Run `L1`–`L5` and record p50/p99 from `oha`.
5. Run `H1`–`H10`, watching `top`/`htop` for CPU saturation and RSS growth.
6. Stop the backend, start the other, repeat from step 2.
