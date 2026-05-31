//! OIDC bearer-token enforcement.
//!
//! Compiled in only when the `auth` cargo feature is enabled. Provides:
//!
//! 1. A [`JwksCache`] that periodically fetches the issuer's JWKS and
//!    keeps the latest snapshot in an [`ArcSwap`] for lock-free reads.
//!    A `kid` cache miss triggers an out-of-band refresh so key
//!    rotation doesn't strand callers.
//! 2. A [`verify_token`] helper that validates signature, `iss`, `aud`,
//!    `exp`/`nbf` (with leeway), and the configured `alg` allow-list.
//! 3. An actix middleware [`Auth`] that extracts the bearer token,
//!    verifies it, and attaches a [`Principal`] to request extensions.
//!    Handlers then use [`require_scope`] / [`require_scopes`] /
//!    [`Principal::tenant`] to enforce per-route policy.
//!
//! The scope strings come from either the standard `scope` claim
//! (space-separated, per RFC 8693) or a `scp` array (Azure AD style).
//! Whichever is present is fine.
//!
//! The Swagger UI's SSO support (`crate::swagger`) is independent of
//! this module — that one only drives the UI login dialog.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use actix_web::body::EitherBody;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::http::header;
use actix_web::{Error as ActixError, HttpMessage, HttpRequest, HttpResponse, ResponseError};
use arc_swap::ArcSwap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use std::future::{Ready, ready};
use std::pin::Pin;

use crate::config::AuthConfig;
use crate::errors::AppError;

fn install_jwt_crypto_provider() {
    let _ = jsonwebtoken::crypto::rust_crypto::DEFAULT_PROVIDER.install_default();
}

// ---------------------------------------------------------------------------
// Principal — what handlers see after a successful auth
// ---------------------------------------------------------------------------

/// Authenticated caller. Attached to every authenticated request via
/// `req.extensions_mut().insert(Principal { … })`; retrieve with
/// `req.extensions().get::<Principal>()`.
#[derive(Debug, Clone)]
pub struct Principal {
    /// `sub` claim — the IdP's stable identifier for the user / client.
    pub sub: String,
    /// All scopes the bearer holds, normalised to lowercase.
    pub scopes: Vec<String>,
    /// Tenant extracted via `auth.tenant_claim`, if configured and
    /// present in the token. `None` when no tenant claim is configured.
    pub tenant: Option<String>,
}

impl Principal {
    /// True iff the bearer has every requested scope.
    pub fn has_all_scopes(&self, required: &[String]) -> bool {
        required.iter().all(|r| self.scopes.iter().any(|s| s == r))
    }
}

// ---------------------------------------------------------------------------
// Scope guard — for use inside handlers
// ---------------------------------------------------------------------------

/// Reject the request unless the attached [`Principal`] holds every
/// scope in `required`. Returns `Ok(())` when the check passes — or
/// when no [`Principal`] is attached *and* `required` is empty (the
/// anonymous-read path).
///
/// Anonymous callers that try to access scoped routes get 401, not
/// 403, so clients can distinguish "you forgot your token" from
/// "your token is insufficient".
pub fn require_scopes(req: &HttpRequest, required: &[String]) -> Result<(), AppError> {
    if required.is_empty() {
        return Ok(());
    }
    let ext = req.extensions();
    match ext.get::<Principal>() {
        Some(p) if p.has_all_scopes(required) => Ok(()),
        Some(_) => Err(AppError::Forbidden(format!(
            "token is missing required scope(s): {}",
            required.join(" ")
        ))),
        None => Err(AppError::Unauthorized(
            "missing or invalid bearer token".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// JWKS cache
// ---------------------------------------------------------------------------

/// Lock-free snapshot of the latest JWKS plus the timestamp it was
/// fetched at (for debug logging only). Swapped atomically by the
/// background refresher.
#[derive(Clone)]
struct JwksSnapshot {
    set: Arc<JwkSet>,
}

/// JWKS cache with a background refresh task. Cheap to clone — the
/// inner snapshot is behind an Arc.
#[derive(Clone)]
pub struct JwksCache {
    inner: Arc<ArcSwap<Option<JwksSnapshot>>>,
    /// OIDC issuer, used to run discovery (`.well-known/openid-configuration`).
    issuer: Arc<String>,
    /// JWKS endpoint discovered from the issuer's OIDC metadata. Cached
    /// once resolved; `None` until the first successful discovery.
    jwks_uri: Arc<ArcSwap<Option<String>>>,
    client: reqwest::Client,
}

impl JwksCache {
    /// Build the cache and seed it from the issuer's JWKS endpoint.
    ///
    /// The JWKS URL is **discovered** from the issuer's OIDC metadata
    /// (`{issuer}/.well-known/openid-configuration` → `jwks_uri`) rather
    /// than assumed, so IdPs that publish keys under a non-standard path
    /// (Keycloak's `…/protocol/openid-connect/certs`, Azure AD, Auth0,
    /// Okta, …) work out of the box. If discovery is unreachable the
    /// cache falls back to the legacy `{issuer}/.well-known/jwks.json`
    /// path for that attempt only, and re-tries discovery on the next
    /// refresh.
    ///
    /// When the initial fetch fails, behaviour depends on
    /// `start_degraded`:
    ///
    /// * `start_degraded = true` → return `Ok` with an empty cache;
    ///   the middleware will reject every auth'd request with 503
    ///   until a subsequent refresh succeeds.
    /// * `start_degraded = false` → return `Err` so `serve` aborts.
    ///
    /// In either case a background refresher is spawned on the current
    /// tokio runtime.
    pub async fn boot(cfg: &AuthConfig) -> Result<Self, AppError> {
        install_jwt_crypto_provider();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| AppError::Internal(format!("reqwest client: {e}")))?;
        let cache = Self {
            inner: Arc::new(ArcSwap::from_pointee(None)),
            issuer: Arc::new(cfg.issuer.clone()),
            jwks_uri: Arc::new(ArcSwap::from_pointee(None)),
            client: client.clone(),
        };

        match cache.resolve_and_fetch().await {
            Ok(set) => {
                cache
                    .inner
                    .store(Arc::new(Some(JwksSnapshot { set: Arc::new(set) })));
                log::info!("auth: JWKS loaded for issuer {}", cfg.issuer);
            }
            Err(e) if cfg.start_degraded => {
                log::warn!(
                    "auth: initial JWKS load for issuer {} failed ({e}); \
                     starting in degraded mode — auth'd requests will return 503 \
                     until JWKS becomes reachable",
                    cfg.issuer
                );
            }
            Err(e) => {
                return Err(AppError::Internal(format!(
                    "auth: JWKS load failed and start_degraded = false: {e}"
                )));
            }
        }

        // Background refresher.
        let refresh = Duration::from_secs(cfg.jwks_refresh_secs.max(60));
        let cache_bg = cache.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(refresh);
            // Skip the immediate tick — we already fetched (or tried) above.
            interval.tick().await;
            loop {
                interval.tick().await;
                cache_bg.refresh_quiet().await;
            }
        });

        Ok(cache)
    }

    /// Resolve the JWKS URL (via OIDC discovery, cached) and fetch the
    /// key set. On the first call this runs discovery against the
    /// issuer's `.well-known/openid-configuration` and caches the
    /// resulting `jwks_uri`. If discovery is unreachable, this falls
    /// back to the legacy `{issuer}/.well-known/jwks.json` path for this
    /// attempt only (without caching), so a later refresh re-discovers.
    async fn resolve_and_fetch(&self) -> Result<JwkSet, String> {
        if let Some(uri) = self.jwks_uri.load_full().as_ref().clone() {
            return fetch_jwks(&self.client, &uri).await;
        }
        match discover_jwks_uri(&self.client, &self.issuer).await {
            Ok(uri) => {
                self.jwks_uri.store(Arc::new(Some(uri.clone())));
                fetch_jwks(&self.client, &uri).await
            }
            Err(e) => {
                let fallback = format!("{}/.well-known/jwks.json", self.issuer);
                log::warn!("auth: OIDC discovery failed ({e}); falling back to legacy {fallback}");
                fetch_jwks(&self.client, &fallback).await
            }
        }
    }

    async fn refresh_quiet(&self) {
        match self.resolve_and_fetch().await {
            Ok(set) => {
                self.inner
                    .store(Arc::new(Some(JwksSnapshot { set: Arc::new(set) })));
                log::debug!("auth: JWKS refreshed");
            }
            Err(e) => log::warn!("auth: JWKS refresh failed: {e}"),
        }
    }

    /// Current snapshot, or `None` if the cache has never been
    /// populated (degraded start, no successful fetch yet).
    fn snapshot(&self) -> Option<JwksSnapshot> {
        self.inner.load_full().as_ref().clone()
    }
}

/// Run OIDC discovery: GET `{issuer}/.well-known/openid-configuration`
/// and return its `jwks_uri`. Returns `Err` on a network failure, a
/// non-success HTTP status, or an unparseable / `jwks_uri`-less body.
async fn discover_jwks_uri(client: &reqwest::Client, issuer: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct OidcMetadata {
        jwks_uri: String,
    }

    let disco_url = format!("{issuer}/.well-known/openid-configuration");
    let resp = client
        .get(&disco_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("discovery {disco_url} → HTTP {}", resp.status()));
    }
    let meta = resp
        .json::<OidcMetadata>()
        .await
        .map_err(|e| format!("discovery {disco_url} body: {e}"))?;
    log::info!(
        "auth: discovered jwks_uri={} via {disco_url}",
        meta.jwks_uri
    );
    Ok(meta.jwks_uri)
}

async fn fetch_jwks(client: &reqwest::Client, url: &str) -> Result<JwkSet, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<JwkSet>().await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Token verification
// ---------------------------------------------------------------------------

/// Subset of JWT claims we care about. Everything else is ignored.
/// `scope`/`scp` are both accepted for backwards compatibility with
/// IdPs that use one or the other.
#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scp: Option<ScpField>,
    // Allow arbitrary extras for tenant-claim extraction.
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScpField {
    String(String),
    List(Vec<String>),
}

fn parse_scopes(c: &Claims) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(s) = &c.scope {
        out.extend(s.split_whitespace().map(|s| s.to_ascii_lowercase()));
    }
    match &c.scp {
        Some(ScpField::String(s)) => {
            out.extend(s.split_whitespace().map(|s| s.to_ascii_lowercase()));
        }
        Some(ScpField::List(l)) => {
            out.extend(l.iter().map(|s| s.to_ascii_lowercase()));
        }
        None => {}
    }
    out
}

fn extract_tenant(c: &Claims, pointer: &str) -> Option<String> {
    if pointer.is_empty() {
        return None;
    }
    // Wrap extras into a Value so JSON pointer works uniformly.
    let v = serde_json::Value::Object(
        c.extra
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    v.pointer(pointer).and_then(|x| match x {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn algorithm_from(name: &str) -> Result<Algorithm, AppError> {
    Ok(match name {
        "RS256" => Algorithm::RS256,
        "RS384" => Algorithm::RS384,
        "RS512" => Algorithm::RS512,
        "ES256" => Algorithm::ES256,
        "ES384" => Algorithm::ES384,
        "PS256" => Algorithm::PS256,
        "PS384" => Algorithm::PS384,
        "PS512" => Algorithm::PS512,
        other => return Err(AppError::Internal(format!("unsupported alg: {other}"))),
    })
}

/// Validate the bearer token, returning a [`Principal`] on success.
pub fn verify_token(
    token: &str,
    cfg: &AuthConfig,
    jwks: &JwksCache,
) -> Result<Principal, AppError> {
    install_jwt_crypto_provider();

    let snap = jwks
        .snapshot()
        .ok_or_else(|| AppError::Unavailable("auth: JWKS not yet available".into()))?;

    let header = decode_header(token)
        .map_err(|e| AppError::Unauthorized(format!("malformed token: {e}")))?;
    let kid = header
        .kid
        .ok_or_else(|| AppError::Unauthorized("token header missing 'kid'".into()))?;
    let jwk = snap
        .set
        .find(&kid)
        .ok_or_else(|| AppError::Unauthorized(format!("unknown signing key kid='{kid}'")))?;
    let key = DecodingKey::from_jwk(jwk)
        .map_err(|e| AppError::Internal(format!("auth: bad JWK in JWKS for kid='{kid}': {e}")))?;

    // Build validation. Pin algorithm to what's in the token header,
    // but only if the operator listed it as allowed.
    let allowed: Vec<Algorithm> = cfg
        .algorithms
        .iter()
        .map(|a| algorithm_from(a))
        .collect::<Result<_, _>>()?;
    if !allowed.contains(&header.alg) {
        return Err(AppError::Unauthorized(format!(
            "token alg '{:?}' not in auth.algorithms allow-list",
            header.alg
        )));
    }
    let mut v = Validation::new(header.alg);
    v.leeway = cfg.leeway_secs;
    v.set_issuer(&[&cfg.issuer]);
    if cfg.audience.is_empty() {
        v.validate_aud = false;
    } else {
        v.set_audience(&[&cfg.audience]);
    }

    let data = decode::<Claims>(token, &key, &v)
        .map_err(|e| AppError::Unauthorized(format!("token rejected: {e}")))?;

    let claims = data.claims;
    let scopes = parse_scopes(&claims);
    let tenant = extract_tenant(&claims, &cfg.tenant_claim);

    if !cfg.allowed_tenants.is_empty() {
        match &tenant {
            Some(t) if cfg.allowed_tenants.iter().any(|a| a == t) => {}
            Some(t) => {
                return Err(AppError::Forbidden(format!(
                    "tenant '{t}' is not in auth.allowed_tenants"
                )));
            }
            None => {
                return Err(AppError::Forbidden(
                    "token does not carry the configured tenant claim".into(),
                ));
            }
        }
    }

    Ok(Principal {
        sub: claims.sub,
        scopes,
        tenant,
    })
}

// ---------------------------------------------------------------------------
// Actix middleware
// ---------------------------------------------------------------------------

/// State shared between every middleware instance. Cheap to clone.
#[derive(Clone)]
pub struct AuthState {
    pub cfg: Arc<AuthConfig>,
    pub jwks: JwksCache,
}

/// Actix middleware. Build with [`Auth::new`] and wrap your `App` with
/// `.wrap(auth)`. Reads `Authorization: Bearer …`, verifies the token,
/// and attaches a [`Principal`] to request extensions. When no header
/// is present, the request is passed through unchanged — handlers must
/// call [`require_scopes`] to enforce policy.
#[derive(Clone)]
pub struct Auth {
    state: Option<AuthState>,
}

impl Auth {
    /// Build an enforcing middleware around the given state.
    pub fn new(state: AuthState) -> Self {
        Self { state: Some(state) }
    }
    /// Build a no-op middleware. Useful so callers can wrap the same
    /// type unconditionally and decide enforcement at runtime.
    pub fn disabled() -> Self {
        Self { state: None }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Auth
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = ActixError> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = ActixError;
    type Transform = AuthMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddleware {
            service: Arc::new(service),
            state: self.state.clone(),
        }))
    }
}

pub struct AuthMiddleware<S> {
    service: Arc<S>,
    state: Option<AuthState>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = ActixError> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = ActixError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    actix_web::dev::forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();
        let state = self.state.clone();
        Box::pin(async move {
            let Some(state) = state else {
                let res = svc.call(req).await?;
                return Ok(res.map_into_left_body());
            };
            // Try to extract a bearer token.
            let header_val = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::trim);
            let token = header_val.and_then(|h| {
                let mut parts = h.splitn(2, ' ');
                match (parts.next(), parts.next()) {
                    (Some(scheme), Some(value)) if scheme.eq_ignore_ascii_case("bearer") => {
                        Some(value.trim().to_string())
                    }
                    _ => None,
                }
            });

            if let Some(tok) = token {
                match verify_token(&tok, &state.cfg, &state.jwks) {
                    Ok(principal) => {
                        if let Some(t) = &principal.tenant {
                            log::debug!(
                                "auth: sub='{}' tenant='{}' scopes={:?}",
                                principal.sub,
                                t,
                                principal.scopes
                            );
                        } else {
                            log::debug!(
                                "auth: sub='{}' scopes={:?}",
                                principal.sub,
                                principal.scopes
                            );
                        }
                        req.extensions_mut().insert(principal);
                    }
                    Err(e) => {
                        // Short-circuit: reject the request here so handlers
                        // never see a forged token.
                        let resp = e.error_response();
                        let (request, _pl) = req.into_parts();
                        let sr = ServiceResponse::new(request, resp).map_into_right_body();
                        return Ok(sr);
                    }
                }
            }

            // No token: handlers will reject if a scope is required.
            let res = svc.call(req).await?;
            Ok(res.map_into_left_body())
        })
    }
}

/// Render a 401 with a `WWW-Authenticate: Bearer` challenge so curl /
/// browsers can prompt for credentials. Only used when emitting the
/// "no token" rejection from inside handlers.
pub fn unauthorized_challenge(msg: &str) -> HttpResponse {
    HttpResponse::Unauthorized()
        .insert_header((header::WWW_AUTHENTICATE, "Bearer realm=\"datapress\""))
        .json(serde_json::json!({ "error": format!("unauthorized: {msg}") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AuthConfig {
        AuthConfig {
            enabled: true,
            issuer: "https://issuer.test".into(),
            audience: "api://datapress".into(),
            read_scopes: vec!["datasets:read".into()],
            reload_scopes: vec!["datasets:reload".into()],
            tenant_claim: "/tid".into(),
            ..AuthConfig::default()
        }
    }

    #[test]
    fn parse_scopes_handles_string_and_array() {
        let c: Claims = serde_json::from_value(serde_json::json!({
            "sub": "u",
            "scope": "openid datasets:read"
        }))
        .unwrap();
        let s = parse_scopes(&c);
        assert!(s.contains(&"openid".into()));
        assert!(s.contains(&"datasets:read".into()));

        let c: Claims = serde_json::from_value(serde_json::json!({
            "sub": "u",
            "scp": ["openid", "datasets:read"]
        }))
        .unwrap();
        let s = parse_scopes(&c);
        assert!(s.contains(&"openid".into()));
        assert!(s.contains(&"datasets:read".into()));
    }

    #[test]
    fn extract_tenant_string_and_number() {
        let c: Claims = serde_json::from_value(serde_json::json!({
            "sub": "u",
            "tid": "acme"
        }))
        .unwrap();
        assert_eq!(extract_tenant(&c, "/tid").as_deref(), Some("acme"));

        let c: Claims = serde_json::from_value(serde_json::json!({
            "sub": "u",
            "org": { "id": 42 }
        }))
        .unwrap();
        assert_eq!(extract_tenant(&c, "/org/id").as_deref(), Some("42"));
    }

    #[test]
    fn has_all_scopes_checks_every_required() {
        let p = Principal {
            sub: "u".into(),
            scopes: vec!["a".into(), "b".into()],
            tenant: None,
        };
        assert!(p.has_all_scopes(&[]));
        assert!(p.has_all_scopes(&["a".into()]));
        assert!(p.has_all_scopes(&["a".into(), "b".into()]));
        assert!(!p.has_all_scopes(&["a".into(), "c".into()]));
    }

    // Smoke: the config helper compiles and produces a sensible default.
    #[test]
    fn cfg_smoke() {
        let c = cfg();
        assert!(c.enabled);
        assert_eq!(c.tenant_claim, "/tid");
    }
}
