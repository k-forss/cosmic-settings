// SPDX-License-Identifier: GPL-3.0-only

//! CalDAV calendar discovery via PROPFIND.

use cosmic_applets_config::calendar::{AuthMethod, CalDavCalendar};
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader;
use std::borrow::Cow;
use std::sync::LazyLock;

use super::secrets::{self, SecretKind};

#[derive(Debug)]
pub enum DiscoveryError {
    Http(String),
    AuthExpired(String),
    Other(String),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(s) | Self::AuthExpired(s) | Self::Other(s) => f.write_str(s),
        }
    }
}

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("cosmic-settings/1.0")
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client")
});

fn get_client(ca_cert_path: Option<&str>) -> Result<Cow<'static, reqwest::Client>, DiscoveryError> {
    match ca_cert_path {
        None => Ok(Cow::Borrowed(&*HTTP_CLIENT)),
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| DiscoveryError::Other(format!("Failed to read CA cert {path}: {e}")))?;
            let cert = reqwest::tls::Certificate::from_pem(&pem)
                .map_err(|e| DiscoveryError::Other(format!("Invalid CA cert PEM: {e}")))?;
            let client = reqwest::Client::builder()
                .user_agent("cosmic-settings/1.0")
                .redirect(reqwest::redirect::Policy::limited(5))
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .add_root_certificate(cert)
                .build()
                .map_err(|e| DiscoveryError::Other(format!("Failed to build client with CA: {e}")))?;
            Ok(Cow::Owned(client))
        }
    }
}

const PROPFIND_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:current-user-principal/>
  </d:prop>
</d:propfind>"#;

const PROPFIND_HOME_SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <c:calendar-home-set/>
  </d:prop>
</d:propfind>"#;

const PROPFIND_CALENDARS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"
            xmlns:cs="http://calendarserver.org/ns/"
            xmlns:ic="http://apple.com/ns/ical/">
  <d:prop>
    <d:resourcetype/>
    <d:displayname/>
    <ic:calendar-color/>
    <cs:getctag/>
  </d:prop>
</d:propfind>"#;

/// Inline credentials for discovery without keyring lookup.
pub enum InlineCredentials {
    None,
    Basic { username: String, password: String },
    Bearer { token: String },
}

/// Discover all calendars on a CalDAV server using inline credentials.
///
/// Unlike `discover_calendars`, this does not read secrets from the keyring.
pub async fn discover_calendars_inline(
    server_url: &str,
    credentials: &InlineCredentials,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalDavCalendar>, DiscoveryError> {
    let client = get_client(ca_cert_path)?;
    let base = server_url.trim_end_matches('/');

    let principal_body =
        propfind_inline(&client, base, "0", PROPFIND_PRINCIPAL, credentials).await?;
    let principal_href = xml_extract_inner(&principal_body, "current-user-principal")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();

    let principal_url = resolve_url(base, &principal_href);

    let home_body =
        propfind_inline(&client, &principal_url, "0", PROPFIND_HOME_SET, credentials).await?;
    let home_href = xml_extract_inner(&home_body, "calendar-home-set")
        .and_then(|block| xml_extract_text(&block, "href"))
        .unwrap_or_default();

    let home_url = resolve_url(base, &home_href);

    let cal_body =
        propfind_inline(&client, &home_url, "1", PROPFIND_CALENDARS, credentials).await?;
    let calendars = parse_calendar_list(&cal_body, base);

    Ok(calendars)
}

/// Discover all calendars on a CalDAV server.
pub async fn discover_calendars(
    server_url: &str,
    auth: &AuthMethod,
    source_id: &str,
    ca_cert_path: Option<&str>,
) -> Result<Vec<CalDavCalendar>, DiscoveryError> {
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

async fn propfind_inline(
    client: &reqwest::Client,
    url: &str,
    depth: &str,
    body: &str,
    credentials: &InlineCredentials,
) -> Result<String, DiscoveryError> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
    let mut request = client
        .request(method, url)
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.to_string());

    request = match credentials {
        InlineCredentials::None => request,
        InlineCredentials::Basic { username, password } => {
            request.basic_auth(username, Some(password))
        }
        InlineCredentials::Bearer { token } => request.bearer_auth(token),
    };

    let response = request
        .send()
        .await
        .map_err(|e| DiscoveryError::Other(format!("PROPFIND request to {url} failed: {e}")))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(DiscoveryError::AuthExpired("form".to_string()));
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
            request.basic_auth(username, Some(password))
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
            request.bearer_auth(token)
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
            request.bearer_auth(token)
        }
        AuthMethod::Oidc {
            has_token: false, ..
        } => {
            return Err(DiscoveryError::AuthExpired(source_id.to_string()));
        }
    })
}

fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if href.starts_with('/') {
        if let Some(origin_end) = base.find("://").map(|i| {
            base[i + 3..]
                .find('/')
                .map_or(base.len(), |slash| i + 3 + slash)
        }) {
            format!("{}{}", &base[..origin_end], href)
        } else {
            format!("{base}{href}")
        }
    } else {
        format!("{base}/{href}")
    }
}

fn parse_calendar_list(xml: &str, base: &str) -> Vec<CalDavCalendar> {
    let mut calendars = Vec::new();

    for block in xml_response_blocks(xml) {
        let is_calendar = block.contains("calendar")
            && block.contains("resourcetype")
            && block.contains("collection");

        if is_calendar {
            let href = xml_extract_text(&block, "href").unwrap_or_default();
            let display_name = xml_extract_text(&block, "displayname").unwrap_or_else(|| {
                href.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("Calendar")
                    .to_string()
            });
            let color = xml_extract_text(&block, "calendar-color");
            let ctag = xml_extract_text(&block, "getctag");

            if !href.is_empty() {
                let full_href = if href.starts_with("http://") || href.starts_with("https://") {
                    href
                } else {
                    resolve_url(base, &href)
                };

                calendars.push(CalDavCalendar {
                    href: full_href,
                    display_name,
                    color,
                    enabled: false,
                    ctag,
                    sync_token: None,
                });
            }
        }
    }

    calendars
}

// ── quick-xml helpers ──────────────────────────────────────────────

fn xml_response_blocks(xml: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth: u32 = 0;
    let mut in_response = false;
    let mut block_buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = local_name(&name_bytes);
                if local == b"response" && !in_response {
                    in_response = true;
                    depth = 1;
                    block_buf.clear();
                    block_buf.extend_from_slice(b"<response>");
                } else if in_response {
                    depth += 1;
                    block_buf.extend_from_slice(b"<");
                    block_buf.extend_from_slice(e.name().as_ref());
                    for attr in e.attributes().flatten() {
                        block_buf.extend_from_slice(b" ");
                        block_buf.extend_from_slice(attr.key.as_ref());
                        block_buf.extend_from_slice(b"=\"");
                        block_buf.extend_from_slice(&attr.value);
                        block_buf.extend_from_slice(b"\"");
                    }
                    block_buf.extend_from_slice(b">");
                }
            }
            Ok(XmlEvent::End(ref e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let local = local_name(&name_bytes);
                if in_response {
                    if local == b"response" && depth == 1 {
                        block_buf.extend_from_slice(b"</response>");
                        if let Ok(s) = String::from_utf8(block_buf.clone()) {
                            blocks.push(s);
                        }
                        in_response = false;
                    } else {
                        block_buf.extend_from_slice(b"</");
                        block_buf.extend_from_slice(e.name().as_ref());
                        block_buf.extend_from_slice(b">");
                        depth -= 1;
                    }
                }
            }
            Ok(XmlEvent::Empty(ref e)) => {
                if in_response {
                    block_buf.extend_from_slice(b"<");
                    block_buf.extend_from_slice(e.name().as_ref());
                    for attr in e.attributes().flatten() {
                        block_buf.extend_from_slice(b" ");
                        block_buf.extend_from_slice(attr.key.as_ref());
                        block_buf.extend_from_slice(b"=\"");
                        block_buf.extend_from_slice(&attr.value);
                        block_buf.extend_from_slice(b"\"");
                    }
                    block_buf.extend_from_slice(b"/>");
                }
            }
            Ok(XmlEvent::Text(ref e)) => {
                if in_response {
                    if let Ok(t) = e.unescape() {
                        block_buf.extend_from_slice(t.as_bytes());
                    }
                }
            }
            Ok(XmlEvent::CData(ref e)) => {
                if in_response {
                    block_buf.extend_from_slice(e.as_ref());
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    blocks
}

fn xml_extract_text(xml: &str, local_name_target: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let target = local_name_target.as_bytes();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                if local_name(e.name().as_ref()) == target {
                    let mut text_buf = Vec::new();
                    match reader.read_event_into(&mut text_buf) {
                        Ok(XmlEvent::Text(t)) => {
                            if let Ok(s) = t.unescape() {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    return Some(trimmed.to_string());
                                }
                            }
                        }
                        Ok(XmlEvent::CData(t)) => {
                            if let Ok(s) = std::str::from_utf8(t.as_ref()) {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    return Some(trimmed.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

fn xml_extract_inner(xml: &str, local_name_target: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let target = local_name_target.as_bytes();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(ref e)) => {
                if local_name(e.name().as_ref()) == target {
                    let mut inner = Vec::new();
                    let mut depth: u32 = 1;
                    let mut inner_buf = Vec::new();
                    loop {
                        match reader.read_event_into(&mut inner_buf) {
                            Ok(XmlEvent::Start(ref ie)) => {
                                depth += 1;
                                inner.extend_from_slice(b"<");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b">");
                            }
                            Ok(XmlEvent::End(ref ie)) => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                inner.extend_from_slice(b"</");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b">");
                            }
                            Ok(XmlEvent::Empty(ref ie)) => {
                                inner.extend_from_slice(b"<");
                                inner.extend_from_slice(ie.name().as_ref());
                                inner.extend_from_slice(b"/>");
                            }
                            Ok(XmlEvent::Text(ref t)) => {
                                if let Ok(s) = t.unescape() {
                                    inner.extend_from_slice(s.as_bytes());
                                }
                            }
                            Ok(XmlEvent::Eof) => break,
                            Err(_) => break,
                            _ => {}
                        }
                        inner_buf.clear();
                    }
                    if let Ok(s) = String::from_utf8(inner) {
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().position(|&b| b == b':') {
        Some(pos) => &qname[pos + 1..],
        None => qname,
    }
}
