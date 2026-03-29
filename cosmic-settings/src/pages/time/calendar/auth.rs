// SPDX-License-Identifier: GPL-3.0-only

//! OIDC authentication flow for CalDAV sources.

use openidconnect::{
    core::{CoreClient, CoreProviderMetadata},
    reqwest::async_http_client,
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, OAuth2TokenResponse,
    PkceCodeChallenge, RedirectUrl, Scope, TokenResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct OidcTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

pub async fn oidc_login(
    issuer_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    scopes: &[String],
) -> Result<OidcTokens, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind callback server: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get callback port: {e}"))?
        .port();
    let redirect_url = format!("http://127.0.0.1:{port}/callback");

    let provider = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer_url.to_string())
                .map_err(|e| format!("Invalid issuer URL: {e}"))?,
            async_http_client,
        ),
    )
    .await
    .map_err(|_| "OIDC discovery timed out (30s)".to_string())?
    .map_err(|e| format!("OIDC discovery failed: {e}"))?;

    let client = CoreClient::from_provider_metadata(
        provider,
        ClientId::new(client_id.to_string()),
        client_secret.map(|s| ClientSecret::new(s.to_string())),
    )
    .set_redirect_uri(
        RedirectUrl::new(redirect_url).map_err(|e| format!("Invalid redirect URL: {e}"))?,
    );

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let nonce = Nonce::new_random();

    let mut auth_request = client.authorize_url(
        openidconnect::AuthenticationFlow::<openidconnect::core::CoreResponseType>::AuthorizationCode,
        CsrfToken::new_random,
        move || nonce.clone(),
    )
    .set_pkce_challenge(pkce_challenge);

    let effective_scopes: Vec<String> = if scopes.is_empty() {
        vec!["openid".to_string(), "offline_access".to_string()]
    } else {
        scopes.to_vec()
    };
    for scope in &effective_scopes {
        auth_request = auth_request.add_scope(Scope::new(scope.clone()));
    }

    let (auth_url, csrf_token, nonce) = auth_request.url();

    if let Err(e) = std::process::Command::new("xdg-open")
        .arg(auth_url.to_string())
        .spawn()
    {
        tracing::warn!("Failed to open browser: {e}");
    }

    let code = wait_for_callback(listener, &csrf_token).await?;

    let token_response = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(pkce_verifier)
            .request_async(async_http_client),
    )
    .await
    .map_err(|_| "Token exchange timed out (30s)".to_string())?
    .map_err(|e| format!("Token exchange failed: {e}"))?;

    if let Some(id_token) = token_response.id_token() {
        let verifier = client.id_token_verifier();
        if let Err(e) = id_token.claims(&verifier, &nonce) {
            tracing::warn!("ID token verification failed: {e}");
        }
    }

    Ok(OidcTokens {
        access_token: token_response.access_token().secret().clone(),
        refresh_token: token_response
            .refresh_token()
            .map(|t: &openidconnect::RefreshToken| t.secret().clone()),
    })
}

async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &CsrfToken,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);

    for _attempt in 0..10 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Authentication timed out (120 s)".to_string());
        }

        let (mut stream, _) = tokio::time::timeout(remaining, listener.accept())
            .await
            .map_err(|_| "Authentication timed out (120 s)".to_string())?
            .map_err(|e| format!("Failed to accept callback: {e}"))?;

        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| format!("Failed to read callback: {e}"))?;
        let request = String::from_utf8_lossy(&buf[..n]);

        let path = match request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        {
            Some(p) => p.to_string(),
            None => {
                let _ = stream.shutdown().await;
                continue;
            }
        };

        let (path_part, query_part) = match path.split_once('?') {
            Some((p, q)) => (p, q),
            None => {
                send_response(&mut stream, "400 Bad Request", "Missing parameters").await;
                continue;
            }
        };

        if !path_part.ends_with("/callback") && path_part != "/callback" {
            send_response(&mut stream, "404 Not Found", "Not the callback endpoint").await;
            continue;
        }

        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(query_part.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();

        let Some(code) = params.get("code") else {
            send_response(&mut stream, "400 Bad Request", "Missing code parameter").await;
            continue;
        };

        let Some(state) = params.get("state") else {
            send_response(&mut stream, "400 Bad Request", "Missing state parameter").await;
            continue;
        };

        if state != expected_state.secret().as_str() {
            send_response(&mut stream, "403 Forbidden", "State mismatch").await;
            return Err("CSRF state mismatch".to_string());
        }

        send_response(
            &mut stream,
            "200 OK",
            "<html><body>\
             <h1>Authentication successful!</h1>\
             <p>You can close this window and return to COSMIC Settings.</p>\
             </body></html>",
        )
        .await;

        return Ok(code.clone());
    }

    Err("Too many invalid callback requests".to_string())
}

async fn send_response(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}
