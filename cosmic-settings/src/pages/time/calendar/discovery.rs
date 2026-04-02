// SPDX-License-Identifier: GPL-3.0-only

//! CalDAV calendar discovery via PROPFIND.
//!
//! Shared helpers (XML parsing, PROPFIND constants, inline credentials) are
//! re-exported from `cosmic_applets_config::calendar::discovery`.

use cosmic_applets_config::calendar::AuthMethod;
use super::secrets::{self, SecretKind};

// Re-export everything from the shared discovery module.
pub use cosmic_applets_config::calendar::discovery::{
    DiscoveryError, InlineCredentials,
    discover_calendars_inline, get_client, propfind_inline,
    resolve_url, parse_calendar_list,
    xml_response_blocks, xml_extract_text, xml_extract_inner, local_name,
    PROPFIND_PRINCIPAL, PROPFIND_HOME_SET, PROPFIND_CALENDARS,
};

/// Discover all calendars on a CalDAV server using keyring-stored credentials.
pub async fn discover_calendars(
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<Vec<cosmic_applets_config::calendar::CalDavCalendar>, DiscoveryError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');

    let principal_body =
        propfind(&client, base, "0", PROPFIND_PRINCIPAL, auth, source_id).await?;
    let principal_href = xml_extract_inner(&principal_body, "current-user-principal")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();
    let principal_url = resolve_url(base, &principal_href);

    let home_body =
        propfind(&client, &principal_url, "0", PROPFIND_HOME_SET, auth, source_id).await?;
    let home_href = xml_extract_inner(&home_body, "calendar-home-set")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();
    let home_url = resolve_url(base, &home_href);

    let cal_body =
        propfind(&client, &home_url, "1", PROPFIND_CALENDARS, auth, source_id).await?;
    let calendars = parse_calendar_list(&cal_body, base);

    Ok(calendars)
}

async fn propfind(
    client: &reqwest::Client,
    url: &str,
    depth: &str,
    body: &str,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<String, DiscoveryError> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
    let mut request = client
        .request(method, url)
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string());

    request = apply_auth(request, auth, source_id).await?;

    let response = request
        .send()
        .await
        .map_err(|e| DiscoveryError::Other(format!("PROPFIND request to {url} failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let body = response.text().await.unwrap_or_default();
        tracing::error!(
            "CalDAV 401 Unauthorized from {url} (source={source_id}): {body}"
        );
        return Err(DiscoveryError::AuthExpired(source_id.to_string()));
    }

    if !response.status().is_success() {
        return Err(DiscoveryError::Other(format!(
            "PROPFIND to {url} returned status {}",
            response.status()
        )));
    }

    response
        .text()
        .await
        .map_err(|e| DiscoveryError::Other(format!("Failed to read PROPFIND response: {e}")))
}

async fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &AuthMethod,
    source_id: &str,
) -> Result<reqwest::RequestBuilder, DiscoveryError> {
    Ok(match auth {
        AuthMethod::None => request,
        AuthMethod::Basic { username } => {
            let password = secrets::load_secret(source_id, SecretKind::Password)
                .await
                .map_err(|e| {
                    DiscoveryError::Other(format!("Failed to load password from keyring: {e}"))
                })?
                .unwrap_or_default();
            request.basic_auth(username, Some(password.as_str()))
        }
        AuthMethod::Bearer => {
            let token = secrets::load_secret(source_id, SecretKind::BearerToken)
                .await
                .map_err(|e| {
                    DiscoveryError::Other(format!("Failed to load bearer token from keyring: {e}"))
                })?
                .ok_or_else(|| {
                    DiscoveryError::Other("Bearer token not found in keyring".into())
                })?;
            request.bearer_auth(token.as_str())
        }
        AuthMethod::Oidc {
            has_token: true, ..
        } => {
            let token = secrets::load_secret(source_id, SecretKind::OidcAccessToken)
                .await
                .map_err(|e| {
                    DiscoveryError::Other(format!("Failed to load OIDC token from keyring: {e}"))
                })?
                .ok_or_else(|| DiscoveryError::AuthExpired(source_id.to_string()))?;
            tracing::debug!(
                "Using OIDC Bearer token from keyring for {source_id}: token successfully loaded"
            );
            request.bearer_auth(token.as_str())
        }
        AuthMethod::Oidc {
            has_token: false, ..
        } => {
            return Err(DiscoveryError::AuthExpired(source_id.to_string()));
        }
    })
}
