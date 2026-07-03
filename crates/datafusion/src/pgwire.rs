//! Optional PostgreSQL wire-protocol front-end for the DataFusion backend.
//!
//! Compiled only when the crate's `pgwire` feature is enabled. It exposes the
//! shared [`SessionContext`] built by [`crate::store::Store`] over the
//! PostgreSQL wire protocol so BI tools (Power BI via Npgsql, `psql`, DBeaver,
//! …) can query the registered datasets directly.
//!
//! Authentication is intentionally minimal. `datafusion-postgres` only exposes
//! a cleartext-password [`AuthSource`](datafusion_postgres::pgwire::api::auth::AuthSource)
//! (SCRAM would require a salted verifier the library does not surface), so:
//!
//! * with a configured password we compose the library's own
//!   [`AuthManager`] / [`DfAuthSource`] with pgwire's
//!   [`CleartextPasswordAuthStartupHandler`], and
//! * without a password we fall back to the library's default `serve()`, which
//!   [`crate::config`] validation only permits on a loopback address.
//!
//! In both cases TLS (when configured) is handled by the library. Config
//! validation additionally forbids exposing a non-loopback listener without
//! TLS, so a cleartext password never crosses a plaintext connection off-box.

use std::sync::Arc;
use std::thread::JoinHandle;

use async_trait::async_trait;
use datafusion::common::ParamValues;
use datafusion::logical_expr::LogicalPlan;
use datafusion::prelude::SessionContext;
use datafusion::sql::sqlparser::ast::Statement;
use tokio::sync::oneshot;

use datafusion_postgres::auth::{AuthManager, DfAuthSource};
use datafusion_postgres::datafusion_pg_catalog::pg_catalog::context::User;
use datafusion_postgres::datafusion_pg_catalog::setup_pg_catalog;
use datafusion_postgres::hooks::HookClient;
use datafusion_postgres::hooks::cursor::CursorStatementHook;
use datafusion_postgres::hooks::set_show::SetShowHook;
use datafusion_postgres::hooks::transactions::TransactionStatementHook;
use datafusion_postgres::pgwire::api::PgWireServerHandlers;
use datafusion_postgres::pgwire::api::ClientInfo;
use datafusion_postgres::pgwire::api::auth::StartupHandler;
use datafusion_postgres::pgwire::api::auth::DefaultServerParameterProvider;
use datafusion_postgres::pgwire::api::auth::cleartext::CleartextPasswordAuthStartupHandler;
use datafusion_postgres::pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use datafusion_postgres::pgwire::api::results::{Response, Tag};
use datafusion_postgres::pgwire::error::PgWireResult;
use datafusion_postgres::{
    DfSessionService, QueryHook, ServerOptions, serve_with_handlers, serve_with_hooks,
};

use datapress_core::config::PgwireConfig;

/// Concrete cleartext-password startup handler used for the authenticated path.
type CleartextStartup =
    CleartextPasswordAuthStartupHandler<DfAuthSource, DefaultServerParameterProvider>;

/// Custom handler set for the authenticated (password) path. Query handling is
/// delegated to the library's [`DfSessionService`]; only the startup handler is
/// swapped for one that enforces cleartext-password auth. All other protocol
/// concerns (copy, error, cancel) keep the trait's default handlers.
struct DatapressHandlers {
    session_service: Arc<DfSessionService>,
    startup: Arc<CleartextStartup>,
}

impl PgWireServerHandlers for DatapressHandlers {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.session_service.clone()
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.session_service.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        self.startup.clone()
    }
}

/// Query hook that swallows the PostgreSQL session-maintenance statements the
/// DataFusion planner does not implement.
///
/// Pooling drivers reset a pooled connection before returning it to the pool by
/// issuing `DISCARD ALL` (Npgsql, used by Power BI) and friends. DataFusion has
/// no planner support for these, so `datafusion-postgres` forwards them and the
/// client gets `XX000: Unsupported SQL statement: DISCARD ALL`, which breaks the
/// pool. We intercept them *before* planning and reply with the matching
/// `CommandComplete` tag and no result set — the same "reasonable no-op"
/// approach the library's own [`TransactionStatementHook`] takes for
/// `BEGIN`/`COMMIT`/`ROLLBACK` (which is why those are deliberately left to it
/// and not handled here).
///
/// Matching is on the parsed statement variant only, never a substring of the
/// SQL text, so a query like `SELECT 'DISCARD ALL'` is passed straight through.
///
/// This behaviour is upstream-worthy for `datafusion-postgres`; until it ships
/// there we carry it locally.
#[derive(Debug)]
struct SessionResetHook;

impl SessionResetHook {
    /// The `CommandComplete` tag to return for a swallowed statement, or `None`
    /// if this hook does not handle the statement.
    fn tag_for(statement: &Statement) -> Option<String> {
        match statement {
            // `DiscardObject`'s `Display` yields ALL/PLANS/SEQUENCES/TEMP, so the
            // tag mirrors real PostgreSQL (`DISCARD ALL`, `DISCARD PLANS`, …).
            Statement::Discard { object_type } => Some(format!("DISCARD {object_type}")),
            // We don't track prepared statements here (the library owns the
            // portal store), so `DEALLOCATE [PREPARE] { ALL | <name> }` is a
            // no-op that just acknowledges success.
            Statement::Deallocate { .. } => Some("DEALLOCATE".to_string()),
            Statement::Reset(_) => Some("RESET".to_string()),
            Statement::UNLISTEN { .. } => Some("UNLISTEN".to_string()),
            _ => None,
        }
    }
}

#[async_trait]
impl QueryHook for SessionResetHook {
    async fn handle_simple_query(
        &self,
        statement: &Statement,
        _session_context: &SessionContext,
        _client: &mut dyn HookClient,
    ) -> Option<PgWireResult<Response>> {
        let tag = Self::tag_for(statement)?;
        // Logged so a future "unsupported statement" report is diagnosable from
        // the server logs; `Statement`'s `Display` prints the normalized SQL.
        log::debug!("pgwire: swallowing session-maintenance statement: {statement}");
        Some(Ok(Response::Execution(Tag::new(&tag))))
    }

    async fn handle_extended_parse_query(
        &self,
        statement: &Statement,
        _session_context: &SessionContext,
        _client: &(dyn ClientInfo + Send + Sync),
    ) -> Option<PgWireResult<LogicalPlan>> {
        // Extended-protocol clients (Npgsql among them) may prepare these too;
        // hand back an empty plan so execution routes through the hook below
        // instead of the planner. Mirrors `TransactionStatementHook`.
        if Self::tag_for(statement).is_some() {
            let schema = datafusion::common::DFSchema::empty();
            return Some(Ok(LogicalPlan::EmptyRelation(
                datafusion::logical_expr::EmptyRelation {
                    produce_one_row: false,
                    schema: Arc::new(schema),
                },
            )));
        }
        None
    }

    async fn handle_extended_query(
        &self,
        statement: &Statement,
        _logical_plan: &LogicalPlan,
        _params: &ParamValues,
        session_context: &SessionContext,
        client: &mut dyn HookClient,
    ) -> Option<PgWireResult<Response>> {
        self.handle_simple_query(statement, session_context, client)
            .await
    }
}

/// Assemble the query hooks installed on every pgwire session.
///
/// This is the library's default set (cursor, `SET`/`SHOW`, transaction
/// statements) plus our [`SessionResetHook`]. `DfSessionService::new` installs
/// the first three by default, but choosing hooks explicitly (via
/// `new_with_hooks`/`serve_with_hooks`) is the only way to add ours, so we
/// re-list the defaults here. The reset hook is purely additive — none of the
/// defaults claim `DISCARD`/`DEALLOCATE`/`RESET`/`UNLISTEN`.
fn query_hooks() -> Vec<Arc<dyn QueryHook>> {
    vec![
        Arc::new(CursorStatementHook),
        Arc::new(SetShowHook),
        Arc::new(TransactionStatementHook),
        Arc::new(SessionResetHook),
    ]
}

/// Per-worker stack size for the dedicated pgwire runtime (32 MiB).
///
/// DataFusion plans and optimizes SQL by recursive descent, and BI clients
/// (DBeaver, DataGrip, Power BI via Npgsql, …) issue very deeply nested
/// `pg_catalog`/`information_schema` introspection queries the moment they
/// connect. Planning those on a default ~2 MiB worker stack — or on the actix
/// runtime's current-thread stack, where a plain `tokio::spawn` would otherwise
/// land — overflows the stack and aborts the *entire* process. Giving pgwire
/// its own generously sized worker stack keeps that recursion in bounds without
/// enlarging every thread in the HTTP server's pool.
const PGWIRE_THREAD_STACK: usize = 32 * 1024 * 1024;

/// Handle to the dedicated OS thread + runtime that hosts the pgwire listener.
///
/// Dropping (or explicitly stopping) the handle signals the listener to stop
/// and joins the thread, so the server is always torn down with its owner.
pub struct PgwireServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for PgwireServer {
    fn drop(&mut self) {
        // Signal the `tokio::select!` in `spawn_pgwire` to fall out of the
        // accept loop, then wait for the runtime (and any in-flight query
        // tasks) to wind down.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the pgwire server on a dedicated OS thread backed by its own
/// multi-thread Tokio runtime whose worker threads use a large stack
/// ([`PGWIRE_THREAD_STACK`]).
///
/// The library's `serve`/`serve_with_handlers` accept loop `tokio::spawn`s each
/// connection onto the *ambient* runtime, so hosting the listener here means
/// every per-connection query task inherits the large-stack workers — which is
/// exactly what keeps DataFusion's recursive planner from overflowing on the
/// deep introspection queries BI tools send. Running on a separate runtime also
/// isolates pgwire from the actix HTTP runtime.
///
/// `ctx` MUST be a clone of the `Store`'s shared context so pgwire clients see
/// the same tables as the HTTP API (see [`serve_pgwire`]).
pub fn spawn_pgwire(ctx: SessionContext, cfg: PgwireConfig) -> std::io::Result<PgwireServer> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (listen, port) = (cfg.listen, cfg.port);

    let thread = std::thread::Builder::new()
        .name("pgwire".to_string())
        .stack_size(PGWIRE_THREAD_STACK)
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("pgwire-worker")
                .thread_stack_size(PGWIRE_THREAD_STACK)
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    log::error!("pgwire: failed to build runtime: {e}");
                    return;
                }
            };

            runtime.block_on(async move {
                log::info!("pgwire: PostgreSQL wire protocol listening on {listen}:{port}");
                tokio::select! {
                    _ = shutdown_rx => {
                        log::info!("pgwire: shutdown signalled; stopping listener");
                    }
                    res = serve_pgwire(ctx, cfg) => {
                        if let Err(e) = res {
                            log::error!("pgwire: server task exited with error: {e}");
                        }
                    }
                }
            });
        })?;

    Ok(PgwireServer {
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

/// Serve the given `SessionContext` over the PostgreSQL wire protocol until the
/// task is cancelled or the listener errors.
///
/// `ctx` MUST be a clone of the `Store`'s shared context (see
/// [`crate::store::Store::session_context`]) so pgwire clients see exactly the
/// same tables as the HTTP API. Never register tables on it here.
pub async fn serve_pgwire(ctx: SessionContext, cfg: PgwireConfig) -> std::io::Result<()> {
    let mut opts = ServerOptions::new()
        .with_host(cfg.listen.to_string())
        .with_port(cfg.port);

    // TLS is wired identically on both auth paths. Config validation guarantees
    // cert/key are set together (or neither), so this all-or-nothing check is
    // sufficient.
    if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        opts = opts
            .with_tls_cert_path(Some(cert.to_string_lossy().into_owned()))
            .with_tls_key_path(Some(key.to_string_lossy().into_owned()));
    }

    let ctx = Arc::new(ctx);

    // Install PostgreSQL catalog emulation on the shared context ONCE, before
    // serving. This registers the `pg_catalog.*` tables plus the postgres
    // compat UDFs (`current_schema`/`current_database`/`pg_get_userbyid`/…)
    // that BI tools (DBeaver, Power BI via Npgsql) and psql's `\dt`/`\d` probe
    // on connect — without them the driver errors out with e.g.
    // "table 'pg_catalog.pg_type' not found". It mutates the shared
    // SessionContext (all clones see it), so it runs here at startup exactly
    // once, never per connection.
    //
    // The `AuthManager` doubles as the catalog's role provider (`pg_roles`,
    // `pg_get_userbyid`); the password path below registers the configured
    // login user on this same instance. The catalog name is read from the
    // session config rather than hardcoded so a future default-catalog change
    // can't silently break introspection.
    let auth_manager = Arc::new(AuthManager::new());
    let default_catalog = ctx
        .copied_config()
        .options()
        .catalog
        .default_catalog
        .clone();
    setup_pg_catalog(&ctx, &default_catalog, auth_manager.clone())
        .map_err(|e| std::io::Error::other(*e))?;

    match &cfg.password {
        // Authenticated path: enforce a single cleartext-over-TLS user built
        // from config. This routes through the library's intended extension
        // point (`serve_with_handlers` + `PgWireServerHandlers`) rather than
        // any hand-rolled protocol logic.
        Some(password) => {
            auth_manager
                .add_user(User {
                    username: cfg.username.clone(),
                    password_hash: password.clone(),
                    roles: Vec::new(),
                    is_superuser: true,
                    can_login: true,
                    connection_limit: None,
                })
                .await
                .map_err(std::io::Error::other)?;

            let auth_source = DfAuthSource::new(auth_manager);
            let startup = Arc::new(CleartextPasswordAuthStartupHandler::new(
                auth_source,
                DefaultServerParameterProvider::default(),
            ));
            let handlers = Arc::new(DatapressHandlers {
                session_service: Arc::new(DfSessionService::new_with_hooks(ctx, query_hooks())),
                startup,
            });

            serve_with_handlers(handlers, &opts).await
        }
        // No password: config validation guarantees a loopback-only listener,
        // so the library's default (permit-all) startup handler is acceptable.
        None => serve_with_hooks(ctx, &opts, query_hooks()).await,
    }
}
