# Errors

Every error response is a JSON envelope:

```json
{ "error": "<short human-readable message>" }
```

The status code carries the structure; the message is for humans, not
machines.

## Status codes

| Status | Meaning                            | Example trigger                                                                                              |
|--------|------------------------------------|--------------------------------------------------------------------------------------------------------------|
| `400`  | Bad request — malformed body or invalid operator | `{"col":"x","op":"unknown"}`, empty `in` array, `aggregations` without `group_by`, unknown column, etc. |
| `403`  | Forbidden                          | Hit `/reload` without the matching `X-Admin-Token`.                                                          |
| `404`  | Not found                          | Unknown dataset name in the URL.                                                                             |
| `413`  | Payload too large                  | Request body exceeded `max_body_bytes`.                                                                      |
| `500`  | Internal server error              | Engine/storage error during query execution.                                                                 |
| `503`  | Service unavailable                | `/readyz` while datasets are still loading; reload while one is in progress.                                 |
| `504`  | Gateway timeout                    | Handler ran longer than `request_timeout_ms`.                                                                |

## Examples

```json
// 400 — empty in() list
{ "error": "predicate 'in' on column 'state' requires a non-empty value array" }

// 400 — unknown column
{ "error": "unknown column: 'sevrity'" }

// 403 — admin token missing/wrong
{ "error": "forbidden" }

// 404 — wrong dataset name
{ "error": "dataset 'accidents_old' not found" }

// 413 — body too big
{ "error": "request payload too large" }

// 503 — not ready
{ "status": "not_ready", "datasets": 0 }

// 504 — handler timeout
{ "error": "request timed out" }
```

## Tuning timeouts and body limits

See [Configuration › Server](../configuration/server.md):
`max_body_bytes`, `request_timeout_ms`, `shutdown_timeout_secs`.
