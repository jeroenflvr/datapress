//! Shared OIDC discovery for the browser-side OAuth2 login flows used by
//! both the Swagger UI (`swagger` feature) and the dataset explorer
//! (`explorer` feature).
//!
//! Both UIs drive an Authorization Code + PKCE flow against an OIDC issuer
//! and attach the acquired bearer token to their API requests. Rather than
//! forcing the browser to fetch the issuer's discovery document
//! client-side (which silently yields an *empty* login dialog when that
//! fetch is blocked by CORS or unreachable), we resolve the authorize /
//! token endpoints once at server startup via [`resolve_oauth2`] and hand
//! the resulting [`ResolvedOAuth2`] to the UI.

use crate::config::SwaggerOAuth2Config;

/// OIDC endpoints + UI parameters resolved from a [`SwaggerOAuth2Config`]
/// at server startup.
///
/// The authorize / token URLs are discovered once at boot via
/// [`resolve_oauth2`]; callers fall back to skipping the UI login (no
/// Authorize / Login button) if discovery fails, rather than shipping a
/// broken dialog.
#[derive(Debug, Clone)]
pub struct ResolvedOAuth2 {
    /// Public OAuth2 client id registered for the UI.
    pub client_id: String,
    /// `authorization_endpoint` from the issuer's OIDC metadata.
    pub authorization_url: String,
    /// `token_endpoint` from the issuer's OIDC metadata.
    pub token_url: String,
    /// Scopes offered in the login dialog (`openid` always included).
    pub scopes: Vec<String>,
    /// Whether to drive the authorization-code flow with PKCE.
    pub pkce: bool,
}

/// Run OIDC discovery for a UI login flow: GET
/// `{issuer}/.well-known/openid-configuration` and pull out the
/// `authorization_endpoint` and `token_endpoint`. Scopes come from the
/// operator's config (with `openid` ensured); the issuer's
/// `scopes_supported` is only used as a fallback when none are
/// configured.
///
/// Returns `Err` on a network failure, a non-success HTTP status, or a
/// metadata body that lacks either endpoint. The issuer's trailing
/// slash (if any) is trimmed so the well-known URL never doubles up.
pub async fn resolve_oauth2(cfg: &SwaggerOAuth2Config) -> Result<ResolvedOAuth2, String> {
    #[derive(serde::Deserialize)]
    struct OidcMetadata {
        authorization_endpoint: Option<String>,
        token_endpoint: Option<String>,
        #[serde(default)]
        scopes_supported: Vec<String>,
    }

    let disco_url = format!(
        "{}/.well-known/openid-configuration",
        cfg.issuer.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
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
    let authorization_url = meta
        .authorization_endpoint
        .ok_or_else(|| format!("discovery {disco_url}: missing authorization_endpoint"))?;
    let token_url = meta
        .token_endpoint
        .ok_or_else(|| format!("discovery {disco_url}: missing token_endpoint"))?;

    let mut scopes = if cfg.scopes.is_empty() {
        meta.scopes_supported
    } else {
        cfg.scopes.clone()
    };
    if !scopes.iter().any(|s| s == "openid") {
        scopes.insert(0, "openid".to_string());
    }

    log::info!("oauth2: OIDC discovery ok (authorize={authorization_url}, token={token_url})");
    Ok(ResolvedOAuth2 {
        client_id: cfg.client_id.clone(),
        authorization_url,
        token_url,
        scopes,
        pkce: cfg.pkce,
    })
}
