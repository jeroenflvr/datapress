#!/usr/bin/env bash
# Generate a small sample parquet file for the materialized datasets demo.
# Requires: duckdb CLI in PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="$SCRIPT_DIR/data"

mkdir -p "$DATA_DIR"

if [ -f "$DATA_DIR/accidents.parquet" ]; then
  echo "Sample data already exists at $DATA_DIR/accidents.parquet — skipping."
  exit 0
fi

echo "Generating sample accidents.parquet..."
duckdb -c "
COPY (
  SELECT
    gen_random_uuid()::VARCHAR AS ID,
    (random() * 3 + 1)::INT AS Severity,
    strftime(CURRENT_DATE - INTERVAL (random() * 365)::INT DAY, '%Y-%m-%d %H:%M:%S') AS Start_Time,
    CASE (row_number() OVER () % 10)
      WHEN 0 THEN 'CA' WHEN 1 THEN 'TX' WHEN 2 THEN 'FL' WHEN 3 THEN 'NY'
      WHEN 4 THEN 'OH' WHEN 5 THEN 'PA' WHEN 6 THEN 'IL' WHEN 7 THEN 'GA'
      WHEN 8 THEN 'NC' ELSE 'WA'
    END AS State,
    CASE (row_number() OVER () % 5)
      WHEN 0 THEN 'Los Angeles' WHEN 1 THEN 'Houston' WHEN 2 THEN 'Miami'
      WHEN 3 THEN 'New York' ELSE 'Columbus'
    END AS City
  FROM range(5000)
) TO '$DATA_DIR/accidents.parquet' (FORMAT PARQUET);
"
echo "Done: $DATA_DIR/accidents.parquet"
