# DataPress — Architecture Brief

DataPress is a small, opinionated, config-driven query server that publishes Parquet and Delta datasets as fast, typed APIs. It belongs to exactly one place in a data platform: the **expose layer**. Ingestion, transformation, and quality remain the job of your pipelines and lakehouse tooling; DataPress picks up where they finish, turning the resulting tables into governed, high-speed APIs for people and applications. Its opinions are what keep it simple: it is read-only, file-backed (Parquet/Delta as the contract with upstream), configuration over code, and deliberately not a database, a transformation engine, or an orchestrator. Instead of standing up a database cluster and building a service per dataset, you point one lightweight process at files on S3 or local disk and it becomes an API — with authentication, metrics, and zero-downtime data refreshes built in. For the business, that means analysts and applications get sub-second access to governed data at the cost of a single small container; for engineering, it means one deployable, one config file, and no bespoke API code per dataset.

## The system at a glance

<img src="/assets/images/architecture/datapress_high_level_overview.svg" alt="DataPress High Level Overview">

Everything runs inside one operating-system process. Three kinds of consumers connect through three protocols — plain HTTP, a DuckDB `ATTACH` extension called Quack, and the native PostgreSQL wire protocol for BI tools — and all of them pass through the same OIDC-based auth layer into the same query engine. The engine (DuckDB or Arrow + DataFusion, chosen in config) serves every request from a single resident copy of the dataset shared by all worker threads, which is why the memory footprint stays flat no matter how many concurrent users arrive. When upstream data changes, a management call triggers a reload: a fresh copy is built from storage and swapped in atomically, so running queries finish on the old data and new queries see the new data — no downtime, no failed requests, no maintenance window. The same engine ships as a standalone binary and container, as a Rust library, and as a Python package that embeds the full server inside your own application — which matters for how little infrastructure the deployments below need.

## How people and tools connect

<img src="/assets/images/architecture/datapress_http_api_clients_detail.svg" alt="DataPress HTTP Clients ">

The HTTP API is the front door for most users. Analysts work from notebooks via the Python client, which returns Arrow tables that flow straight into pandas or Polars; BI platforms and anything on the JVM connect through the official JDBC driver — a read-only Type-4 driver published to Maven Central as [`org.datap-rs:datapress-jdbc`](https://mvnrepository.com/artifact/org.datap-rs/datapress-jdbc) (Apache 2.0), added to a project as a single dependency with no native libraries to install; engineers script against the CLI or call the API directly; and every running instance serves its own browser Explorer and Swagger console for ad-hoc inspection. Two response formats cover the spectrum: JSON for applications and dashboards, Arrow IPC for data tooling where serialization overhead matters. Access is governed by standard bearer tokens from your identity provider — read access and data-refresh rights are separate scopes, so consuming data never implies the right to change it.

## Running it on AWS (or other cloud providers)

### Starter deployment

<img src="/assets/images/architecture/aws-basic.svg" alt="DataPress Basic deployment">

The minimum viable production setup is three managed pieces: an Application Load Balancer terminating TLS and health-checking the server's readiness endpoint, a single ECS Fargate task running the official container, and the S3 bucket holding the data. With the DuckDB backend streaming data lazily from S3, a 2–4 GB task serves a substantial dataset, and the monthly cost is that of one small container. Data refreshes are a one-line API call after each pipeline run. This is the right starting point for a team's first datasets — it is production-grade, just manually operated.

### Enterprise deployment

<img src="/assets/images/architecture/aws-enterprise.svg" alt="DataPress Enterprise deployment">

The enterprise variant keeps the same core and automates the operations around it. Data pipelines publish an event to a Kafka topic (MSK Serverless) whenever they finish writing to S3, and the reload happens automatically, so published data is never stale and no human is in the loop. Notably, this does not require a sidecar container or a separate controller deployment: because DataPress also ships as a Python package that embeds the entire server in-process, a single Python application starts the server, consumes the Kafka topic, and calls the dataset reload itself — one container, one process, one thing to operate and observe. Security moves to central systems of record: S3 credentials are fetched from HashiCorp Vault at startup rather than stored in task definitions, and all access — end users, BI tools, and the reload controller alike — authenticates through the corporate OIDC identity provider with scoped tokens. Run the service with multiple tasks across availability zones and the load balancer provides seamless failover.

Choosing between the two is not an either/or architecture decision: the enterprise version is the starter version plus automation and governance, so teams typically launch with the starter layout and add the Kafka, Vault, and IdP integrations as datasets multiply and audit requirements arrive.