---
description: >-
  datapress-jdbc: a read-only, Type-4 (pure Java) JDBC driver for a DataPress
  server. Published on Maven Central. Connect BI tools like DBeaver and DataGrip
  over standard java.sql, streaming results as Apache Arrow IPC.
---

# JDBC driver (`datapress-jdbc`)

[`datapress-jdbc`](https://github.com/jeroenflvr/datapress-jdbc) is a read-only,
Type‑4 (**pure Java, no native components**) JDBC driver for a running DataPress
server. It wraps the [HTTP + Arrow IPC API](../reference/endpoints.md) behind the
standard `java.sql` interfaces, so BI tools, SQL consoles, and JVM applications
can talk to DataPress without knowing anything about its REST endpoints.

- **Read-only and analytics-oriented** — `SELECT`, `WITH … SELECT`, and
  `DESCRIBE` only.
- **Arrow-native** — results stream as Apache Arrow IPC and are decoded lazily,
  so large result sets use roughly constant memory.
- **Tool compatible** — DBeaver, DataGrip, and other clients see a synthesized
  [`DatabaseMetaData`](https://docs.oracle.com/en/java/javase/17/docs/api/java.sql/java/sql/DatabaseMetaData.html)
  (catalogs, schemas, tables, columns) rather than any SQL rewriting.

!!! note "Requires the raw-SQL endpoint"
    The driver issues SQL against the server's [`POST /api/v1/sql`](../query/sql.md)
    endpoint, which is **disabled by default**. Enable it server-side with
    `[sql].enabled = true` (or `sql_enabled=True` when launching from Python)
    before connecting.

## Install

The driver is published on **Maven Central** as
`org.datap-rs:datapress-jdbc:0.1.0`.

=== "Maven"

    ```xml
    <dependency>
      <groupId>org.datap-rs</groupId>
      <artifactId>datapress-jdbc</artifactId>
      <version>0.1.0</version>
    </dependency>
    ```

=== "Gradle (Kotlin DSL)"

    ```kotlin
    implementation("org.datap-rs:datapress-jdbc:0.1.0")
    ```

=== "Gradle (Groovy)"

    ```groovy
    implementation 'org.datap-rs:datapress-jdbc:0.1.0'
    ```

For GUI tools, download the self-contained (shaded) jar directly — it bundles a
relocated Arrow, Jackson, and FlatBuffers, so no extra jars are needed:

<https://repo1.maven.org/maven2/org/datap-rs/datapress-jdbc/0.1.0/datapress-jdbc-0.1.0.jar>

!!! warning "JDK 16+ JVM flag"
    Arrow's off-heap memory needs the `java.nio` module opened. Add this to the
    JVM running the driver (your application, DBeaver, DataGrip, …):

    ```
    --add-opens=java.base/java.nio=ALL-UNNAMED
    ```

## Connection URL

```text
jdbc:datapress://host[:port][/][?token=<bearer>&tls=false&...]
```

| Part      | Meaning                                                                 |
| --------- | ----------------------------------------------------------------------- |
| `host`    | DataPress server host.                                                   |
| `port`    | Optional server port.                                                    |
| `token`   | Bearer token for a server with [auth](../operations/auth.md) enabled. May also be supplied as the JDBC `password` / a `token` connection property. |
| `tls`     | Set `tls=false` to talk plain HTTP to a non-TLS server.                  |

The driver class is `org.datapress.jdbc.DataPressDriver`. On a modern JDBC
runtime it registers itself via the service loader — no `Class.forName(...)`
needed.

## Java example

```java
import java.sql.*;

public class Demo {
    public static void main(String[] args) throws Exception {
        String url = "jdbc:datapress://127.0.0.1:8000/?tls=false";

        try (Connection conn = DriverManager.getConnection(url);
             Statement stmt = conn.createStatement();
             ResultSet rs = stmt.executeQuery(
                 "SELECT State, COUNT(*) AS n FROM accidents GROUP BY State ORDER BY n DESC")) {

            while (rs.next()) {
                System.out.printf("%s: %d%n", rs.getString("State"), rs.getLong("n"));
            }
        }
    }
}
```

With auth enabled, pass the token via properties instead of the URL:

```java
java.util.Properties props = new java.util.Properties();
props.setProperty("token", System.getenv("DATAPRESS_TOKEN"));
Connection conn = DriverManager.getConnection("jdbc:datapress://my-host:8000/", props);
```

## Using it with DBeaver

The driver exposes tables and columns through `DatabaseMetaData`, so DBeaver's
database navigator, data preview, and SQL editor all work.

### 1. Register the driver

**Database → Driver Manager → New**, then set:

| Field         | Value                               |
| ------------- | ----------------------------------- |
| Driver Name   | `DataPress`                         |
| Class Name    | `org.datapress.jdbc.DataPressDriver`|
| URL Template  | `jdbc:datapress://{host}:{port}/`   |
| Default Port  | `8000`                              |

Under **Libraries**, either **Add Artifact** with the Maven coordinates
`org.datap-rs:datapress-jdbc:0.1.0` (DBeaver downloads it from Maven Central),
or **Add File** and select the shaded jar you downloaded above. "Find Class"
should then resolve `org.datapress.jdbc.DataPressDriver`. Click **OK**.

### 2. Create a connection

- **JDBC URL:** `jdbc:datapress://127.0.0.1:8000/` (append `?tls=false` for a
  plain-HTTP server).
- Leave user/password empty for a server without auth. If your server requires a
  bearer [token](../operations/auth.md), add it as a connection
  property named `token` (or as the `password`).
- **Test Connection** should succeed — the driver performs a `GET /version`
  handshake.

### 3. What you should see

Expand the connection in the navigator:

- Catalog `datapress` → Schema `main`.
- Your datasets appear as tables; expanding a table's **Columns** shows the
  mapped SQL types (e.g. `BIGINT`, `VARCHAR`, `BOOLEAN`, `DOUBLE`,
  `TIMESTAMP WITH TIME ZONE`).
- Double-click a table → **Data** tab to preview rows, or open a **SQL Editor**
  and run a `SELECT`.

## Limitations (by design)

- **Read-only.** Only `SELECT` / `WITH … SELECT` / `DESCRIBE` are accepted.
- **One dataset per SQL statement** — matches the server's raw-SQL endpoint
  (no cross-dataset joins yet).
- **No server-side parameter binding.** `PreparedStatement` substitutes
  parameters client-side.
- The server's **raw-SQL endpoint must be enabled** (`[sql].enabled = true`).

## See also

- [Raw SQL endpoint](../query/sql.md) — the API the driver talks to, and how to
  enable it.
- [Authentication](../operations/auth.md) — issuing and passing bearer
  tokens.
- Source and issues: <https://github.com/jeroenflvr/datapress-jdbc>
