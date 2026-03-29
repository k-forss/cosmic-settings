// SPDX-License-Identifier: GPL-3.0-only

//! Secure secret storage via the freedesktop Secret Service API (D-Bus).

use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;

const APP_LABEL: &str = "com.system76.CosmicAppletTime";

#[derive(Debug, Clone, Copy)]
pub enum SecretKind {
    Password,
    BearerToken,
    OidcAccessToken,
    OidcRefreshToken,
    OidcClientSecret,
}

impl SecretKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::BearerToken => "bearer_token",
            Self::OidcAccessToken => "oidc_access_token",
            Self::OidcRefreshToken => "oidc_refresh_token",
            Self::OidcClientSecret => "oidc_client_secret",
        }
    }
}

fn attributes(source_id: &str, kind: SecretKind) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APP_LABEL),
        ("source_id", source_id),
        ("secret_kind", kind.as_str()),
    ])
}

pub async fn store_secret(
    source_id: &str,
    kind: SecretKind,
    secret: &str,
) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let collection = ss
        .get_default_collection()
        .await
        .map_err(|e| format!("Failed to get default keyring: {e}"))?;

    if collection.is_locked().await.unwrap_or(true) {
        collection
            .unlock()
            .await
            .map_err(|e| format!("Failed to unlock keyring: {e}"))?;
    }

    let label = format!("COSMIC Calendar – {source_id} – {}", kind.as_str());
    let attrs = attributes(source_id, kind);

    collection
        .create_item(&label, attrs, secret.as_bytes(), true, "text/plain")
        .await
        .map_err(|e| format!("Failed to store secret: {e}"))?;

    Ok(())
}

pub async fn load_secret(source_id: &str, kind: SecretKind) -> Result<Option<String>, String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let attrs = attributes(source_id, kind);
    let results = ss
        .search_items(attrs)
        .await
        .map_err(|e| format!("Failed to search keyring: {e}"))?;

    let item = match results.unlocked.first() {
        Some(item) => item,
        None => match results.locked.first() {
            Some(item) => {
                item.unlock()
                    .await
                    .map_err(|e| format!("Failed to unlock item: {e}"))?;
                item
            }
            None => return Ok(None),
        },
    };

    let secret_bytes = item
        .get_secret()
        .await
        .map_err(|e| format!("Failed to read secret: {e}"))?;

    String::from_utf8(secret_bytes)
        .map(Some)
        .map_err(|e| format!("Secret is not valid UTF-8: {e}"))
}

pub async fn delete_secrets(source_id: &str) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|e| format!("Failed to connect to Secret Service: {e}"))?;

    let kinds = [
        SecretKind::Password,
        SecretKind::BearerToken,
        SecretKind::OidcAccessToken,
        SecretKind::OidcRefreshToken,
        SecretKind::OidcClientSecret,
    ];

    for kind in kinds {
        let attrs = attributes(source_id, kind);
        if let Ok(results) = ss.search_items(attrs).await {
            for item in results.unlocked.iter().chain(results.locked.iter()) {
                let _ = item.delete().await;
            }
        }
    }

    Ok(())
}
