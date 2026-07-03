# `pg_catalog` emulation returns zero rows for Npgsql's connect-time type-load query

Upstream: `datafusion-pg-catalog` (verified against `0.17.2`, the latest release)
served through `datafusion-postgres 0.17.0`.

## Symptom

`Npgsql` (the .NET PostgreSQL driver behind **Power BI**, DataGrip's PG driver,
and many `dotnet` apps) loads its client-side type map with a single large query
against `pg_type` / `pg_namespace` / `pg_proc` the moment a connection is opened
(`NpgsqlConnection.Open()`). Against the emulated catalog that query returns
**zero rows**. Npgsql accepts the empty result silently, leaving its type map
empty, and then fails on the *first* real result set with:

```
The field 'table_schema' has a type currently unknown to Npgsql (OID 25)
```

or, on a bare `SELECT 1`:

```
System.InvalidCastException: Can't cast database type .<unknown> to Int32
```

A silent empty catalog result is worse than an error: the failure surfaces far
from its cause.

## Reproduction (no Windows / Power BI required)

A `net8.0` console app pinned to **Npgsql 4.0.17** (the version Power BI ships;
8.x/9.x take a different type-load path):

```csharp
using Npgsql;
using var conn = new NpgsqlConnection(
    "Host=127.0.0.1;Port=5432;Username=u;Database=db;SSL Mode=Disable");
conn.Open();                                   // "OPENED OK"
using var cmd = new NpgsqlCommand("SELECT 1", conn);
using var r = cmd.ExecuteReader();
while (r.Read()) Console.WriteLine(r.GetInt32(0));   // throws: .<unknown> -> Int32
```

The exact SQL Npgsql sends at open (captured from
`RUST_LOG=datafusion_postgres=debug`, `Received execute extended query`):

```sql
SELECT ns.nspname, a.typname, a.oid, a.typrelid, a.typbasetype,
  CASE WHEN pg_proc.proname = 'array_recv' THEN 'a' ELSE a.typtype END AS type,
  CASE WHEN pg_proc.proname = 'array_recv' THEN a.typelem
       WHEN a.typtype = 'r' THEN rngsubtype ELSE 0 END AS elemoid,
  CASE WHEN pg_proc.proname IN ('array_recv', 'oidvectorrecv') THEN 3
       WHEN a.typtype = 'r' THEN 2 WHEN a.typtype = 'd' THEN 1 ELSE 0 END AS ord
FROM pg_catalog.pg_type AS a
JOIN pg_catalog.pg_namespace AS ns ON (ns.oid = a.typnamespace)
JOIN pg_catalog.pg_proc ON pg_proc.oid = a.typreceive
LEFT OUTER JOIN pg_catalog.pg_class AS cls ON (cls.oid = a.typrelid)
LEFT OUTER JOIN pg_catalog.pg_type AS b ON (b.oid = a.typelem)
LEFT OUTER JOIN pg_catalog.pg_class AS elemcls ON (elemcls.oid = b.typrelid)
LEFT OUTER JOIN pg_catalog.pg_range ON (pg_range.rngtypid = a.oid)
WHERE a.typtype IN ('b', 'r', 'e', 'd')
   OR (a.typtype = 'c' AND cls.relkind = 'c')
   OR (pg_proc.proname = 'array_recv' AND (b.typtype IN ('b', 'r', 'e', 'd')
        OR (b.typtype = 'p' AND b.typname IN ('record', 'void'))
        OR (b.typtype = 'c' AND elemcls.relkind = 'c')))
   OR (a.typtype = 'p' AND a.typname IN ('record', 'void'))
ORDER BY ord;
```

Running that query via `psql` against the emulated server returns **0 rows**;
against a real `postgres:14` it returns **367**.

## Root cause — two independent linkage bugs (either alone empties the result)

Decomposing the joins one at a time against the emulated catalog:

| step | emulated | postgres:14 |
|------|---------:|------------:|
| `pg_type` rows | 617 | — |
| `pg_proc` rows | 3330 | — |
| `pg_type JOIN pg_namespace ON ns.oid = a.typnamespace` | **0** | many |
| `pg_type JOIN pg_proc ON pg_proc.oid = a.typreceive` | **0** | many |

1. **`pg_namespace.oid` vs `pg_type.typnamespace` mismatch.**
   `pg_type` is emulated with PostgreSQL's *fixed* catalog OIDs — `int4` has
   `typnamespace = 11` (`pg_catalog`) — but `pg_namespace.oid` is generated from
   a **runtime counter** (`src/pg_catalog/pg_namespace.rs`: `oid_counter.fetch_add`),
   so `pg_catalog` comes out as e.g. `16458`. The inner join
   `ns.oid = a.typnamespace` (`11` vs `16458`) matches nothing.

   ```text
   emulated pg_namespace:  16457|public   16458|pg_catalog
   emulated pg_type.int4:  typnamespace = 11          <-- points at nothing
   postgres:14 pg_type.int4: typnamespace = 11, pg_namespace.pg_catalog.oid = 11
   ```

2. **`pg_type.typreceive` stores a name, not a `regproc` OID.**
   Emulated `int4.typreceive = 'int4recv'` (the function *name*); real PostgreSQL
   stores the `regproc` **OID** `2406` (which merely *displays* as `int4recv`).
   The inner join `pg_proc.oid = a.typreceive` therefore compares an integer OID
   against a string and matches nothing. (Joining on `pg_proc.proname =
   a.typreceive` instead yields 597 rows — the data is present but keyed wrong.)

Both bugs affect **any** client that joins `pg_type` to `pg_namespace`/`pg_proc`,
not just Npgsql; Npgsql just happens to depend on it at connect time.

## Suggested upstream fix

Make the emulated `pg_type` and its neighbours self-consistent, e.g.:

- give the built-in schemas (`pg_catalog`, `information_schema`, `public`) their
  fixed OIDs in `pg_namespace` (`pg_catalog = 11`) so `typnamespace` resolves, and
- populate `pg_type.typreceive` with the `pg_proc` OID (matching `typname||'recv'`)
  rather than the function name, so `regproc` joins work.

## Local workaround shipped downstream

Until the emulation is fixed we repair the query at the pgwire hook layer with a
`QueryHook` (`NpgsqlTypeLoadHook`) whose `handle_extended_parse_query` recognizes
the query by the verbatim broken predicate
`JOIN pg_catalog.pg_proc ON pg_proc.oid = a.typreceive` and applies two surgical
substitutions before planning, preserving every projected column and its order:

- `JOIN pg_catalog.pg_namespace AS ns ON (ns.oid = a.typnamespace)` →
  an inline derived table mapping the two catalog OIDs `pg_type` actually uses to
  their names:
  `JOIN (SELECT 11 AS oid, 'pg_catalog' AS nspname UNION ALL SELECT 13283 AS oid, 'information_schema' AS nspname) AS ns ON (ns.oid = a.typnamespace)`
- `JOIN pg_catalog.pg_proc ON pg_proc.oid = a.typreceive` →
  `JOIN pg_catalog.pg_proc ON pg_proc.proname = a.typreceive`

The rewrite is gated on the fingerprint so no other query is touched. After it,
the query returns 375 rows including the base types (`bool`, `int2/4/8`,
`float4/8`, `numeric`, `text`, `varchar`, `date`, `timestamp[tz]`, `uuid`, …) and
their array types (`_int4` with `elemoid = 23`), and the Npgsql console app
succeeds: `OPENED OK`, `SELECT 1 => 1`, parameterized `SELECT @p::int4 => 42`.
