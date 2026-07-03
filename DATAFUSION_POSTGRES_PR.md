# Add a built-in hook for session-maintenance statements (`DISCARD` / `DEALLOCATE` / `RESET` / `UNLISTEN`)

## Problem

Pooling PostgreSQL clients reset a connection before returning it to the pool by
issuing session-maintenance statements — most importantly `DISCARD ALL`.
**Npgsql** (the driver **Power BI** uses) does this on every pool reset, and so
do many other drivers/pools.

`datafusion-postgres` has no handling for these statements, so they fall through
to the DataFusion planner, which rejects them:

```
XX000: This feature is not implemented: Unsupported SQL statement: DISCARD ALL
```

Because the failure happens on the *reset* path, the symptom is intermittent and
pool-dependent: the first query on a fresh connection succeeds, the pool then
recycles the connection, the reset fails, and the driver tears the whole pool
down. From the user's side (e.g. Power BI) it looks like a random connection
failure. `psql` never reproduces it because it doesn't send `DISCARD ALL`.

## Why the hook layer is the right place

`DfSessionService::do_query` already runs the registered `QueryHook`s *before*
planning, and only calls `session_context.sql(...)` if no hook claims the
statement:

```text
parse → for each statement:
          for hook in query_hooks:
              if hook.handle_simple_query(...) == Some(resp): use resp; continue
          else: session_context.sql(query)   // ← planner, errors on DISCARD ALL
```

The crate already ships hooks for cursors, `SET`/`SHOW`, and transactions
(`TransactionStatementHook` responds to `BEGIN`/`COMMIT`/`ROLLBACK` with
reasonable no-ops without opening a real transaction). Session-reset statements
are the same class of "protocol bookkeeping the engine doesn't model" and belong
in the same place.

## Proposed change

Add a `SessionResetHook` under `src/hooks/` and include it in the default hook
set returned by `DfSessionService::new`.

### `src/hooks/session_reset.rs` (new)

Matching is on the **parsed `sqlparser` `Statement` variant**, never a substring
of the SQL text — so a query such as `SELECT 'DISCARD ALL'` (string literal) or
`SELECT reset_col FROM t` (identifier) is passed straight through.

```rust
use datafusion::logical_expr::LogicalPlan;
use datafusion::prelude::SessionContext;
use datafusion::sql::sqlparser::ast::Statement;

use async_trait::async_trait;
use datafusion::common::ParamValues;
use pgwire::api::ClientInfo;
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;

use crate::QueryHook;
use crate::hooks::HookClient;

/// Hook for PostgreSQL session-maintenance statements that DataFusion cannot
/// plan but pooling drivers (Npgsql/Power BI, etc.) rely on:
///
/// * `DISCARD { ALL | PLANS | SEQUENCES | TEMP | TEMPORARY }`
/// * `DEALLOCATE [PREPARE] { ALL | <name> }`
/// * `RESET { ALL | <configuration_parameter> }`
/// * `UNLISTEN { <channel> | * }`
///
/// Like [`TransactionStatementHook`], it does not perform real work — it just
/// acknowledges the statement with the correct `CommandComplete` tag and no
/// result set. `BEGIN`/`COMMIT`/`ROLLBACK` are intentionally left to
/// `TransactionStatementHook`.
#[derive(Debug)]
pub struct SessionResetHook;

impl SessionResetHook {
    /// The `CommandComplete` tag to return for a swallowed statement, or `None`
    /// if this hook does not handle the statement.
    fn tag_for(statement: &Statement) -> Option<String> {
        match statement {
            // `DiscardObject`'s `Display` yields ALL/PLANS/SEQUENCES/TEMP, so the
            // tag mirrors real PostgreSQL (`DISCARD ALL`, `DISCARD PLANS`, …).
            Statement::Discard { object_type } => Some(format!("DISCARD {object_type}")),
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
        log::debug!("swallowing session-maintenance statement: {statement}");
        Some(Ok(Response::Execution(Tag::new(&tag))))
    }

    async fn handle_extended_parse_query(
        &self,
        statement: &Statement,
        _session_context: &SessionContext,
        _client: &(dyn ClientInfo + Send + Sync),
    ) -> Option<PgWireResult<LogicalPlan>> {
        // Extended-protocol clients may prepare these too; hand back an empty
        // plan so execution routes through the hook below, not the planner.
        // Mirrors `TransactionStatementHook`.
        if Self::tag_for(statement).is_some() {
            let schema = datafusion::common::DFSchema::empty();
            return Some(Ok(LogicalPlan::EmptyRelation(
                datafusion::logical_expr::EmptyRelation {
                    produce_one_row: false,
                    schema: std::sync::Arc::new(schema),
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
```

> **Note on `DEALLOCATE <name>`:** this hook treats it as a no-op acknowledgment
> because prepared statements are owned by the portal store, not the hook. If we
> want `DEALLOCATE` to actually drop tracked statements, that can be a follow-up
> once the hook has access to the portal store; the no-op is already correct for
> the pool-reset use case (drivers issue `DEALLOCATE ALL` defensively and only
> need a success reply).

### `src/hooks/mod.rs`

```diff
 pub mod cursor;
 pub mod permissions;
+pub mod session_reset;
 pub mod set_show;
 pub mod transactions;
```

### `src/handlers.rs` — add to the default hook set

```diff
 impl DfSessionService {
     pub fn new(session_context: Arc<SessionContext>) -> DfSessionService {
         let hooks: Vec<Arc<dyn QueryHook>> = vec![
             Arc::new(CursorStatementHook),
             Arc::new(SetShowHook),
             Arc::new(TransactionStatementHook),
+            Arc::new(SessionResetHook),
         ];
         Self::new_with_hooks(session_context, hooks)
     }
```

Because it's additive to the default set, existing users get the fix for free
via `serve` / `serve_with_handlers`, and no one who builds a custom hook vec is
affected.

## Behavior: before vs. after

| Statement sent by client | Before | After |
|---|---|---|
| `DISCARD ALL` | `XX000: Unsupported SQL statement` | `CommandComplete DISCARD ALL` |
| `DISCARD PLANS` | error | `CommandComplete DISCARD PLANS` |
| `DEALLOCATE ALL` | error | `CommandComplete DEALLOCATE` |
| `RESET ALL` | error | `CommandComplete RESET` |
| `UNLISTEN *` | error | `CommandComplete UNLISTEN` |
| `SELECT 'DISCARD ALL'` | returns `'DISCARD ALL'` | returns `'DISCARD ALL'` (**unchanged**) |
| `BEGIN` / `COMMIT` / `ROLLBACK` | handled by transactions hook | handled by transactions hook (**unchanged**) |

## Tests

Unit test in `src/hooks/session_reset.rs` (mirroring the existing hook tests):

```rust
#[tokio::test]
async fn swallows_session_reset_statements_but_not_string_literals() {
    let ctx = SessionContext::new();
    let hook = SessionResetHook;

    // Each utility statement is claimed with the right tag.
    for (sql, expect_tag) in [
        ("DISCARD ALL", "DISCARD ALL"),
        ("DISCARD PLANS", "DISCARD PLANS"),
        ("DEALLOCATE ALL", "DEALLOCATE"),
        ("RESET ALL", "RESET"),
        ("UNLISTEN *", "UNLISTEN"),
    ] {
        let stmt = parse_one(sql);
        assert_eq!(SessionResetHook::tag_for(&stmt).as_deref(), Some(expect_tag), "{sql}");
    }

    // A string literal that merely contains the keyword is NOT claimed.
    let stmt = parse_one("SELECT 'DISCARD ALL'");
    assert!(SessionResetHook::tag_for(&stmt).is_none());
}
```

An end-to-end test (in the crate's server tests) should, over the simple
protocol, send `DISCARD ALL` / `RESET ALL` / `DEALLOCATE ALL` / `UNLISTEN *`,
assert each succeeds, run a follow-up `SELECT` on the same connection to prove it
is still usable, and assert `SELECT 'DISCARD ALL'` returns the literal.

## Manual reproduction (no Windows required)

An Npgsql console app with pooling enabled that opens two connections
sequentially forces the pool to reset one connection — reproducing the failure
before this change and succeeding after. `psql` alone does not reproduce it.

## Notes

- sqlparser (0.61 at time of writing) models every one of these as a dedicated
  `Statement` variant (`Discard { object_type }`, `Deallocate { name, prepare }`,
  `Reset(ResetStatement)`, `UNLISTEN { channel }`), so no string parsing is
  needed and over-matching is impossible.
- This was first implemented downstream in the DataPress project as a local hook
  layered on top of `serve_with_hooks`; upstreaming it removes the need for every
  integrator to re-derive the same fix.
