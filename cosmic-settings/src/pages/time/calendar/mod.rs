// SPDX-License-Identifier: GPL-3.0-only

//! Calendar settings page for COSMIC Settings.
//!
//! Manages calendar sources (CalDAV, ICS URL, ICS File), authentication,
//! per-calendar configuration, and color assignment.

mod auth;
mod discovery;
mod secrets;

use std::sync::Arc;

#[cfg(feature = "xdg-portal")]
use cosmic::dialog::file_chooser;

use cosmic::{
    Apply, Task,
    cosmic_config::{self, CosmicConfigEntry},
    iced::Length,
    widget::{self, settings, space},
};
use cosmic_applets_config::calendar::{
    AuthMethod, CalDavCalendar, CalendarConfig, EncryptionMode, SourceConfig, SourceType,
    CALENDAR_CONFIG_ID,
};
use cosmic_settings_page::{self as page, Section, section};
use slotmap::SlotMap;
use tracing::error;
use zeroize::Zeroizing;

/// Color presets for calendar sources/calendars.
const COLOR_PRESETS: &[&str] = &[
    "#1a73e8", "#d50000", "#e67c73", "#f4511e", "#f6bf26", "#33b679", "#0b8043", "#039be5",
    "#7986cb", "#8e24aa", "#616161", "#795548",
];

pub struct Page {
    entity: page::Entity,
    calendar_config: CalendarConfig,
    config_handle: Option<cosmic_config::Config>,
    // Source add/edit form state
    editing: bool,
    editing_source_id: Option<String>,
    form_name: String,
    form_url: String,
    form_source_type: usize,
    form_auth_method: usize,
    form_username: String,
    form_password: String,
    form_token: String,
    form_color: String,
    form_oidc_issuer: String,
    form_oidc_client_id: String,
    form_oidc_client_secret: String,
    form_oidc_scopes: String,
    form_ca_cert_path: String,
    // Form-level discovery state (two-stage CalDAV add)
    form_discovered_calendars: Vec<CalDavCalendar>,
    form_discovering: bool,
    form_discovery_error: Option<String>,
    form_source_id: String,
    form_oidc_logged_in: bool,
    form_editing_cal_color: Option<usize>,
    form_cal_color_input: String,
    // Discovery state (for already-saved sources)
    discovering_source: Option<String>,
    discovery_error: Option<String>,
    /// Sources for which we already attempted a token refresh (prevents loops).
    refresh_attempted: std::collections::HashSet<String>,
    // Per-calendar color editing
    editing_calendar_color: Option<(String, String)>, // (source_id, cal_href)
    calendar_color_input: String,
}

impl Default for Page {
    fn default() -> Self {
        let config_handle = cosmic_config::Config::new(CALENDAR_CONFIG_ID, 1).ok();
        let calendar_config = config_handle
            .as_ref()
            .and_then(|c| CalendarConfig::get_entry(c).ok())
            .unwrap_or_default();

        Self {
            entity: page::Entity::default(),
            calendar_config,
            config_handle,
            editing: false,
            editing_source_id: None,
            form_name: String::new(),
            form_url: String::new(),
            form_source_type: 0,
            form_auth_method: 0,
            form_username: String::new(),
            form_password: String::new(),
            form_token: String::new(),
            form_color: String::from("#1a73e8"),
            form_oidc_issuer: String::new(),
            form_oidc_client_id: String::new(),
            form_oidc_client_secret: String::new(),
            form_oidc_scopes: String::from("openid, profile, email, offline_access"),
            form_ca_cert_path: String::new(),
            form_discovered_calendars: Vec::new(),
            form_discovering: false,
            form_discovery_error: None,
            form_source_id: format!("form-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()),
            form_oidc_logged_in: false,
            form_editing_cal_color: None,
            form_cal_color_input: String::new(),
            discovering_source: None,
            discovery_error: None,
            refresh_attempted: std::collections::HashSet::new(),
            editing_calendar_color: None,
            calendar_color_input: String::new(),
        }
    }
}

impl page::Page<crate::pages::Message> for Page {
    fn set_id(&mut self, entity: page::Entity) {
        self.entity = entity;
    }

    fn content(
        &self,
        sections: &mut SlotMap<section::Entity, Section<crate::pages::Message>>,
    ) -> Option<page::Content> {
        Some(vec![
            sections.insert(sources_section()),
            sections.insert(source_form_section()),
            sections.insert(sync_settings_section()),
        ])
    }

    fn info(&self) -> page::Info {
        page::Info::new("time-calendar", "x-office-calendar-symbolic")
            .title(fl!("calendar"))
            .description(fl!("calendar-desc"))
    }

    fn on_enter(&mut self) -> Task<crate::pages::Message> {
        // Re-read config on enter in case the applet changed it
        if let Some(ref handle) = self.config_handle {
            if let Ok(config) = CalendarConfig::get_entry(handle) {
                self.calendar_config = config;
            }
        }
        Task::none()
    }
}

impl page::AutoBind<crate::pages::Message> for Page {}

impl Page {
    fn save_config(&self) {
        if let Some(ref handle) = self.config_handle {
            if let Err(e) = self.calendar_config.write_entry(handle) {
                error!("Failed to save calendar config: {e}");
            }
        }
    }

    fn reset_form(&mut self) {
        self.editing = false;
        self.editing_source_id = None;
        self.form_name.clear();
        self.form_url.clear();
        self.form_source_type = 0;
        self.form_auth_method = 0;
        self.form_username.clear();
        self.form_password.clear();
        self.form_token.clear();
        self.form_color = String::from("#1a73e8");
        self.form_oidc_issuer.clear();
        self.form_oidc_client_id.clear();
        self.form_oidc_client_secret.clear();
        self.form_oidc_scopes = String::from("openid, profile, email, offline_access");
        self.form_ca_cert_path.clear();
        self.form_discovered_calendars.clear();
        self.form_discovering = false;
        self.form_discovery_error = None;
        self.form_source_id = format!("form-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos());
        self.form_oidc_logged_in = false;
        self.form_editing_cal_color = None;
        self.form_cal_color_input.clear();
    }

    fn fill_form_from_source(&mut self, source: &SourceConfig) {
        self.form_name = source.name.clone();
        self.form_color = source.color.clone();
        self.form_ca_cert_path = source.ca_cert_path.clone().unwrap_or_default();
        match &source.source_type {
            SourceType::CalDav { url, auth, .. } => {
                self.form_source_type = 0;
                self.form_url = url.clone();
                self.fill_auth_form(auth);
            }
            SourceType::IcsUrl { url, auth } => {
                self.form_source_type = 1;
                self.form_url = url.clone();
                self.fill_auth_form(auth);
            }
            SourceType::IcsFile { path } => {
                self.form_source_type = 2;
                self.form_url = path.clone();
                self.form_auth_method = 0;
            }
        }
    }

    fn fill_auth_form(&mut self, auth: &AuthMethod) {
        match auth {
            AuthMethod::None => {
                self.form_auth_method = 0;
            }
            AuthMethod::Basic { username } => {
                self.form_auth_method = 1;
                self.form_username = username.clone();
                self.form_password.clear();
            }
            AuthMethod::Bearer => {
                self.form_auth_method = 2;
                self.form_token.clear();
            }
            AuthMethod::Oidc {
                issuer_url,
                client_id,
                scopes,
                ..
            } => {
                self.form_auth_method = 3;
                self.form_oidc_issuer = issuer_url.clone();
                self.form_oidc_client_id = client_id.clone();
                self.form_oidc_client_secret.clear();
                self.form_oidc_scopes = scopes.join(", ");
            }
        }
    }

    fn form_valid(&self) -> bool {
        let name_valid = !self.form_name.trim().is_empty();
        let url_valid = match self.form_source_type {
            0 | 1 => {
                let u = self.form_url.trim();
                u.starts_with("http://") || u.starts_with("https://")
            }
            2 => {
                let p = self.form_url.trim();
                !p.is_empty() && std::path::Path::new(p).exists()
            }
            _ => !self.form_url.trim().is_empty(),
        };
        name_valid && url_valid
    }

    pub fn update(&mut self, message: Message) -> Task<crate::Message> {
        match message {
            // ── Source management ───────────────────────────────
            Message::AddSource => {
                self.reset_form();
                self.editing = true;
            }

            Message::EditSource(source_id) => {
                if let Some(source) = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                {
                    let source = source.clone();
                    self.reset_form();
                    self.editing = true;
                    self.editing_source_id = Some(source_id);
                    self.fill_form_from_source(&source);
                }
            }

            Message::RemoveSource(source_id) => {
                self.calendar_config.sources.retain(|s| s.id != source_id);
                self.save_config();
                tokio::spawn(async move {
                    if let Err(e) = secrets::delete_secrets(&source_id).await {
                        tracing::warn!("Failed to clean up keyring secrets: {e}");
                    }
                });
            }

            Message::ToggleSource(source_id) => {
                if let Some(src) = self
                    .calendar_config
                    .sources
                    .iter_mut()
                    .find(|s| s.id == source_id)
                {
                    src.enabled = !src.enabled;
                }
                self.save_config();
            }

            Message::SaveSource => {
                if !self.form_valid() {
                    return Task::none();
                }

                let scopes: Vec<String> = self
                    .form_oidc_scopes
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let auth = match self.form_auth_method {
                    1 => AuthMethod::Basic {
                        username: self.form_username.clone(),
                    },
                    2 => AuthMethod::Bearer,
                    3 => AuthMethod::Oidc {
                        issuer_url: self.form_oidc_issuer.clone(),
                        client_id: self.form_oidc_client_id.clone(),
                        has_token: self.form_oidc_logged_in,
                        has_client_secret: !self.form_oidc_client_secret.is_empty(),
                        scopes,
                    },
                    _ => AuthMethod::None,
                };
                let source_type = match self.form_source_type {
                    0 => SourceType::CalDav {
                        url: self.form_url.clone(),
                        auth,
                        calendars: std::mem::take(&mut self.form_discovered_calendars),
                    },
                    1 => SourceType::IcsUrl {
                        url: self.form_url.clone(),
                        auth,
                    },
                    _ => SourceType::IcsFile {
                        path: self.form_url.clone(),
                    },
                };

                let mut source =
                    SourceConfig::new(self.form_name.clone(), self.form_color.clone(), source_type);
                // For OIDC: reuse the form_source_id so secrets stored during login are found
                if self.form_oidc_logged_in {
                    source.id = self.form_source_id.clone();
                }
                if !self.form_ca_cert_path.trim().is_empty() {
                    source.ca_cert_path = Some(self.form_ca_cert_path.clone());
                }
                let source_id = source.id.clone();
                self.calendar_config.sources.push(source);
                self.save_config();

                let password = self.form_password.clone();
                let token = self.form_token.clone();
                let client_secret = self.form_oidc_client_secret.clone();
                let auth_method = self.form_auth_method;
                let oidc_logged_in = self.form_oidc_logged_in;
                self.reset_form();

                return cosmic::Task::future(async move {
                    let result = match auth_method {
                        1 => secrets::store_secret(
                            &source_id,
                            secrets::SecretKind::Password,
                            &password,
                        )
                        .await,
                        2 => secrets::store_secret(
                            &source_id,
                            secrets::SecretKind::BearerToken,
                            &token,
                        )
                        .await,
                        3 if !client_secret.is_empty() && !oidc_logged_in => {
                            secrets::store_secret(
                                &source_id,
                                secrets::SecretKind::OidcClientSecret,
                                &client_secret,
                            )
                            .await
                        }
                        _ => Ok(()),
                    };
                    if let Err(e) = result {
                        error!("Failed to store secret in keyring: {e}");
                    }
                    Message::ConfigReload
                })
                .map(crate::pages::Message::Calendar)
                .map(crate::Message::PageMessage);
            }

            Message::SaveEditSource => {
                let Some(ref source_id) = self.editing_source_id else {
                    return Task::none();
                };
                if !self.form_valid() {
                    return Task::none();
                }
                let source_id = source_id.clone();

                // Detect auth method change for cleanup
                let old_auth_index = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                    .map(|s| match &s.source_type {
                        SourceType::CalDav { auth, .. } | SourceType::IcsUrl { auth, .. } => {
                            match auth {
                                AuthMethod::None => 0u8,
                                AuthMethod::Basic { .. } => 1,
                                AuthMethod::Bearer => 2,
                                AuthMethod::Oidc { .. } => 3,
                            }
                        }
                        SourceType::IcsFile { .. } => 0,
                    });
                let auth_changed = old_auth_index
                    .map(|old| old != self.form_auth_method as u8)
                    .unwrap_or(false);

                let scopes: Vec<String> = self
                    .form_oidc_scopes
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let auth = match self.form_auth_method {
                    1 => AuthMethod::Basic {
                        username: self.form_username.clone(),
                    },
                    2 => AuthMethod::Bearer,
                    3 => {
                        let existing_has_token = self
                            .calendar_config
                            .sources
                            .iter()
                            .find(|s| s.id == source_id)
                            .and_then(|s| match &s.source_type {
                                SourceType::CalDav {
                                    auth: AuthMethod::Oidc { has_token, .. },
                                    ..
                                }
                                | SourceType::IcsUrl {
                                    auth: AuthMethod::Oidc { has_token, .. },
                                    ..
                                } => Some(*has_token),
                                _ => None,
                            })
                            .unwrap_or(false);
                        AuthMethod::Oidc {
                            issuer_url: self.form_oidc_issuer.clone(),
                            client_id: self.form_oidc_client_id.clone(),
                            has_token: existing_has_token,
                            has_client_secret: !self.form_oidc_client_secret.is_empty(),
                            scopes,
                        }
                    }
                    _ => AuthMethod::None,
                };

                if let Some(src) = self
                    .calendar_config
                    .sources
                    .iter_mut()
                    .find(|s| s.id == source_id)
                {
                    src.name = self.form_name.clone();
                    src.color = self.form_color.clone();
                    src.ca_cert_path = if self.form_ca_cert_path.trim().is_empty() {
                        None
                    } else {
                        Some(self.form_ca_cert_path.clone())
                    };
                    match self.form_source_type {
                        0 => {
                            if let SourceType::CalDav {
                                url: u, auth: a, ..
                            } = &mut src.source_type
                            {
                                *u = self.form_url.clone();
                                *a = auth;
                            } else {
                                src.source_type = SourceType::CalDav {
                                    url: self.form_url.clone(),
                                    auth,
                                    calendars: Vec::new(),
                                };
                            }
                        }
                        1 => {
                            if let SourceType::IcsUrl {
                                url: u, auth: a, ..
                            } = &mut src.source_type
                            {
                                *u = self.form_url.clone();
                                *a = auth;
                            } else {
                                src.source_type = SourceType::IcsUrl {
                                    url: self.form_url.clone(),
                                    auth,
                                };
                            }
                        }
                        _ => {
                            src.source_type = SourceType::IcsFile {
                                path: self.form_url.clone(),
                            };
                        }
                    }
                }
                self.save_config();

                let password = self.form_password.clone();
                let token = self.form_token.clone();
                let client_secret = self.form_oidc_client_secret.clone();
                let auth_method = self.form_auth_method;
                self.reset_form();

                return cosmic::Task::future(async move {
                    if auth_changed {
                        if let Err(e) = secrets::delete_secrets(&source_id).await {
                            error!("Failed to clean up old secrets: {e}");
                        }
                    }
                    let result = match auth_method {
                        1 if !password.is_empty() => secrets::store_secret(
                            &source_id,
                            secrets::SecretKind::Password,
                            &password,
                        )
                        .await,
                        2 if !token.is_empty() => secrets::store_secret(
                            &source_id,
                            secrets::SecretKind::BearerToken,
                            &token,
                        )
                        .await,
                        3 if !client_secret.is_empty() => secrets::store_secret(
                            &source_id,
                            secrets::SecretKind::OidcClientSecret,
                            &client_secret,
                        )
                        .await,
                        _ => Ok(()),
                    };
                    if let Err(e) = result {
                        error!("Failed to store secret in keyring: {e}");
                    }
                    Message::ConfigReload
                })
                .map(crate::pages::Message::Calendar)
                .map(crate::Message::PageMessage);
            }

            Message::CancelForm => {
                self.reset_form();
            }

            // ── Form field updates ─────────────────────────────
            Message::FormName(v) => self.form_name = v,
            Message::FormUrl(v) => self.form_url = v,
            Message::FormSourceType(v) => self.form_source_type = v,
            Message::FormAuth(v) => self.form_auth_method = v,
            Message::FormUsername(v) => self.form_username = v,
            Message::FormPassword(v) => self.form_password = v,
            Message::FormToken(v) => self.form_token = v,
            Message::FormColor(v) => self.form_color = v,
            Message::FormOidcIssuer(v) => self.form_oidc_issuer = v,
            Message::FormOidcClientId(v) => self.form_oidc_client_id = v,
            Message::FormOidcClientSecret(v) => self.form_oidc_client_secret = v,
            Message::FormOidcScopes(v) => self.form_oidc_scopes = v,
            Message::FormCaCertPath(v) => self.form_ca_cert_path = v,

            Message::BrowseCaCert => {
                #[cfg(feature = "xdg-portal")]
                return cosmic::Task::future(async move {
                    let result = file_chooser::open::Dialog::new()
                        .title(fl!("calendar-source-ca-cert"))
                        .modal(false)
                        .open_file()
                        .await;
                    let mapped = result
                        .map(|resp| resp.url().to_owned())
                        .map_err(|e| e.to_string());
                    Message::CaCertBrowsed(Arc::new(mapped))
                })
                .map(crate::pages::Message::Calendar)
                .map(crate::Message::PageMessage);
            }

            Message::CaCertBrowsed(result) => {
                if let Ok(url) = Arc::into_inner(result).unwrap_or(Err(String::new())) {
                    if let Ok(path) = url.to_file_path() {
                        self.form_ca_cert_path = path.to_string_lossy().into_owned();
                    }
                }
            }

            // ── Form-level CalDAV discovery (two-stage add) ────
            Message::FormDiscover => {
                if !self.form_valid() || self.form_source_type != 0 {
                    return Task::none();
                }
                self.form_discovering = true;
                self.form_discovery_error = None;

                let url = self.form_url.clone();
                let ca = if self.form_ca_cert_path.trim().is_empty() {
                    None
                } else {
                    Some(self.form_ca_cert_path.clone())
                };

                // Build inline credentials from form fields
                let credentials = match self.form_auth_method {
                    1 => discovery::InlineCredentials::Basic {
                        username: self.form_username.clone(),
                        password: self.form_password.clone(),
                    },
                    2 => discovery::InlineCredentials::Bearer {
                        token: self.form_token.clone(),
                    },
                    3 => {
                        // OIDC: must have logged in first; use stored access token
                        if !self.form_oidc_logged_in {
                            self.form_discovering = false;
                            self.form_discovery_error = Some(
                                fl!("calendar-form-oidc-login-first"),
                            );
                            return Task::none();
                        }
                        let sid = self.form_source_id.clone();
                        return cosmic::Task::future(async move {
                            let token = secrets::load_secret(
                                &sid,
                                secrets::SecretKind::OidcAccessToken,
                            )
                            .await
                            .ok()
                            .flatten()
                            .map(|z| String::from(z.as_str()))
                            .unwrap_or_default();
                            let creds = discovery::InlineCredentials::Bearer { token };
                            match discovery::discover_calendars_inline(
                                &url,
                                &creds,
                                ca.as_deref(),
                            )
                            .await
                            {
                                Ok(cals) => Message::FormDiscovered(cals),
                                Err(e) => Message::FormDiscoverError(e.to_string()),
                            }
                        })
                        .map(crate::pages::Message::Calendar)
                        .map(crate::Message::PageMessage);
                    }
                    _ => discovery::InlineCredentials::None,
                };

                return cosmic::Task::future(async move {
                    match discovery::discover_calendars_inline(
                        &url,
                        &credentials,
                        ca.as_deref(),
                    )
                    .await
                    {
                        Ok(cals) => Message::FormDiscovered(cals),
                        Err(e) => Message::FormDiscoverError(e.to_string()),
                    }
                })
                .map(crate::pages::Message::Calendar)
                .map(crate::Message::PageMessage);
            }

            Message::FormDiscovered(calendars) => {
                self.form_discovering = false;
                self.form_discovery_error = None;
                self.form_discovered_calendars = calendars;
            }

            Message::FormDiscoverError(err) => {
                self.form_discovering = false;
                self.form_discovery_error = Some(err);
            }

            Message::FormToggleCalendar(idx) => {
                if let Some(cal) = self.form_discovered_calendars.get_mut(idx) {
                    cal.enabled = !cal.enabled;
                }
            }

            Message::FormCalendarColor(idx) => {
                let current = self
                    .form_discovered_calendars
                    .get(idx)
                    .map(|c| c.color.clone())
                    .unwrap_or_else(|| "#1a73e8".to_string());
                self.form_cal_color_input = current;
                self.form_editing_cal_color = Some(idx);
            }

            Message::FormCalColorInput(v) => {
                self.form_cal_color_input = v;
            }

            Message::FormCalColorPreset(hex) => {
                self.form_cal_color_input = hex.clone();
                if let Some(idx) = self.form_editing_cal_color {
                    if let Some(cal) = self.form_discovered_calendars.get_mut(idx) {
                        cal.color = hex;
                    }
                }
            }

            Message::FormCalColorSave => {
                if let Some(idx) = self.form_editing_cal_color {
                    if let Some(cal) = self.form_discovered_calendars.get_mut(idx) {
                        cal.color = self.form_cal_color_input.clone();
                    }
                }
                self.form_editing_cal_color = None;
            }

            Message::FormCalColorCancel => {
                self.form_editing_cal_color = None;
            }

            Message::FormOidcLogin => {
                let issuer = self.form_oidc_issuer.clone();
                let client_id = self.form_oidc_client_id.clone();
                let client_secret = if self.form_oidc_client_secret.is_empty() {
                    None
                } else {
                    Some(self.form_oidc_client_secret.clone())
                };
                let scopes: Vec<String> = self
                    .form_oidc_scopes
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                return cosmic::Task::future(async move {
                    let result = auth::oidc_login(
                        &issuer,
                        &client_id,
                        client_secret.as_deref(),
                        &scopes,
                    )
                    .await
                    .map(|t| (t.access_token, t.refresh_token));
                    Message::FormOidcLoginResult(result)
                })
                .map(crate::pages::Message::Calendar)
                .map(crate::Message::PageMessage);
            }

            Message::FormOidcLoginResult(result) => match result {
                Ok((access_token, refresh_token)) => {
                    self.form_oidc_logged_in = true;

                    // ── Auto-save the source after successful OIDC login ──
                    if self.form_valid() && self.form_source_type == 0 {
                        let scopes: Vec<String> = self
                            .form_oidc_scopes
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let auth = AuthMethod::Oidc {
                            issuer_url: self.form_oidc_issuer.clone(),
                            client_id: self.form_oidc_client_id.clone(),
                            has_token: true,
                            has_client_secret: !self.form_oidc_client_secret.is_empty(),
                            scopes,
                        };
                        let source_type = SourceType::CalDav {
                            url: self.form_url.clone(),
                            auth,
                            calendars: Vec::new(),
                        };
                        let mut source = SourceConfig::new(
                            self.form_name.clone(),
                            self.form_color.clone(),
                            source_type,
                        );
                        source.id = self.form_source_id.clone();
                        if !self.form_ca_cert_path.trim().is_empty() {
                            source.ca_cert_path = Some(self.form_ca_cert_path.clone());
                        }
                        let source_id = source.id.clone();
                        let client_secret = self.form_oidc_client_secret.clone();

                        self.calendar_config.sources.push(source);
                        self.save_config();
                        self.reset_form();

                        return cosmic::Task::future(async move {
                            if let Err(e) = secrets::store_secret(
                                &source_id,
                                secrets::SecretKind::OidcAccessToken,
                                &access_token,
                            )
                            .await
                            {
                                error!("Failed to store OIDC access token: {e}");
                            }
                            if let Some(rt) = &refresh_token {
                                if let Err(e) = secrets::store_secret(
                                    &source_id,
                                    secrets::SecretKind::OidcRefreshToken,
                                    rt,
                                )
                                .await
                                {
                                    error!("Failed to store OIDC refresh token: {e}");
                                }
                            }
                            if !client_secret.is_empty() {
                                if let Err(e) = secrets::store_secret(
                                    &source_id,
                                    secrets::SecretKind::OidcClientSecret,
                                    &client_secret,
                                )
                                .await
                                {
                                    error!("Failed to store OIDC client secret: {e}");
                                }
                            }
                            Message::OidcSavedAndDiscover(source_id, access_token)
                        })
                        .map(crate::pages::Message::Calendar)
                        .map(crate::Message::PageMessage);
                    }

                    // Fallback: form not valid, just store tokens for later manual save
                    let sid = self.form_source_id.clone();
                    return cosmic::Task::future(async move {
                        if let Err(e) = secrets::store_secret(
                            &sid,
                            secrets::SecretKind::OidcAccessToken,
                            &access_token,
                        )
                        .await
                        {
                            error!("Failed to store OIDC access token: {e}");
                        }
                        if let Some(rt) = &refresh_token {
                            if let Err(e) = secrets::store_secret(
                                &sid,
                                secrets::SecretKind::OidcRefreshToken,
                                rt,
                            )
                            .await
                            {
                                error!("Failed to store OIDC refresh token: {e}");
                            }
                        }
                        Message::FormDiscover
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
                Err(e) => {
                    error!("Form OIDC login failed: {e}");
                    self.form_discovery_error = Some(e);
                }
            },

            // ── OIDC auto-save complete: enter edit view + discover ──
            Message::OidcSavedAndDiscover(source_id, _access_token) => {
                // Enter edit mode for the newly saved source
                if let Some(source) = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                {
                    let source = source.clone();
                    self.reset_form();
                    self.editing = true;
                    self.editing_source_id = Some(source_id.clone());
                    self.fill_form_from_source(&source);
                }
                // Discover via keyring roundtrip (tokens already stored)
                return self.update(Message::Discover(source_id));
            }

            // ── CalDAV discovery (saved sources) ───────────────
            Message::Discover(source_id) => {
                self.discovering_source = Some(source_id.clone());
                self.discovery_error = None;
                let source = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id);
                if let Some(SourceConfig {
                    source_type: SourceType::CalDav { url, auth, .. },
                    ca_cert_path,
                    ..
                }) = source
                {
                    let url = url.clone();
                    let auth = auth.clone();
                    let ca = ca_cert_path.clone();
                    let sid = source_id;
                    return cosmic::Task::future(async move {
                        match discovery::discover_calendars(&url, &auth, &sid, ca.as_deref()).await
                        {
                            Ok(cals) => Message::Discovered(sid, cals),
                            Err(e) => Message::DiscoverError(sid, e.to_string()),
                        }
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
            }

            Message::Discovered(source_id, discovered) => {
                self.discovering_source = None;
                if let Some(src) = self
                    .calendar_config
                    .sources
                    .iter_mut()
                    .find(|s| s.id == source_id)
                {
                    if let SourceType::CalDav { calendars, .. } = &mut src.source_type {
                        for new_cal in discovered {
                            if !calendars.iter().any(|c| c.href == new_cal.href) {
                                calendars.push(new_cal);
                            }
                        }
                    }
                }
                self.refresh_attempted.remove(&source_id);
                self.save_config();
            }

            Message::DiscoverError(source_id, err) => {
                self.discovering_source = None;
                error!("Discovery failed for {source_id}: {err}");

                // If auth expired and source uses OIDC, try refreshing the token
                if err.contains("Authentication expired") || err.contains("expired") {
                    let is_oidc = self
                        .calendar_config
                        .sources
                        .iter()
                        .find(|s| s.id == source_id)
                        .map(|s| matches!(&s.source_type,
                            SourceType::CalDav { auth: AuthMethod::Oidc { .. }, .. }
                            | SourceType::IcsUrl { auth: AuthMethod::Oidc { .. }, .. }
                        ))
                        .unwrap_or(false);

                    if is_oidc && !self.refresh_attempted.contains(&source_id) {
                        self.refresh_attempted.insert(source_id.clone());
                        return self.update(Message::OidcRefresh(source_id));
                    }
                }

                self.discovery_error = Some(err);
            }

            Message::ToggleCalendar(source_id, cal_href) => {
                if let Some(src) = self
                    .calendar_config
                    .sources
                    .iter_mut()
                    .find(|s| s.id == source_id)
                {
                    if let SourceType::CalDav { calendars, .. } = &mut src.source_type {
                        if let Some(cal) = calendars.iter_mut().find(|c| c.href == cal_href) {
                            cal.enabled = !cal.enabled;
                        }
                    }
                }
                self.save_config();
            }

            // ── Per-calendar color ─────────────────────────────
            Message::EditCalendarColor(source_id, cal_href) => {
                // Get current color as default
                let current = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                    .and_then(|s| {
                        if let SourceType::CalDav { calendars, .. } = &s.source_type {
                            calendars
                                .iter()
                                .find(|c| c.href == cal_href)
                                .map(|c| c.color.clone())
                                .or_else(|| Some(s.color.clone()))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "#1a73e8".to_string());
                self.calendar_color_input = current;
                self.editing_calendar_color = Some((source_id, cal_href));
            }

            Message::CalendarColorInput(v) => {
                self.calendar_color_input = v;
            }

            Message::CalendarColorPreset(hex) => {
                self.calendar_color_input = hex.clone();
                // Apply immediately
                if let Some((ref source_id, ref cal_href)) = self.editing_calendar_color {
                    if let Some(src) = self
                        .calendar_config
                        .sources
                        .iter_mut()
                        .find(|s| s.id == *source_id)
                    {
                        if let SourceType::CalDav { calendars, .. } = &mut src.source_type {
                            if let Some(cal) = calendars.iter_mut().find(|c| c.href == *cal_href) {
                                cal.color = hex;
                            }
                        }
                    }
                    self.save_config();
                }
            }

            Message::SaveCalendarColor => {
                if let Some((ref source_id, ref cal_href)) = self.editing_calendar_color {
                    if let Some(src) = self
                        .calendar_config
                        .sources
                        .iter_mut()
                        .find(|s| s.id == *source_id)
                    {
                        if let SourceType::CalDav { calendars, .. } = &mut src.source_type {
                            if let Some(cal) = calendars.iter_mut().find(|c| c.href == *cal_href) {
                                cal.color = self.calendar_color_input.clone();
                            }
                        }
                    }
                    self.save_config();
                }
                self.editing_calendar_color = None;
            }

            Message::CancelCalendarColor => {
                self.editing_calendar_color = None;
            }

            // ── OIDC login ─────────────────────────────────────
            Message::OidcLogin(source_id) => {
                let source = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id);
                let oidc_info = source.and_then(|s| match &s.source_type {
                    SourceType::CalDav {
                        auth:
                            AuthMethod::Oidc {
                                issuer_url,
                                client_id,
                                has_client_secret,
                                scopes,
                                ..
                            },
                        ..
                    }
                    | SourceType::IcsUrl {
                        auth:
                            AuthMethod::Oidc {
                                issuer_url,
                                client_id,
                                has_client_secret,
                                scopes,
                                ..
                            },
                        ..
                    } => Some((
                        issuer_url.clone(),
                        client_id.clone(),
                        *has_client_secret,
                        scopes.clone(),
                    )),
                    _ => None,
                });
                if let Some((issuer, client, needs_secret, scopes)) = oidc_info {
                    let sid = source_id;
                    return cosmic::Task::future(async move {
                        let secret = if needs_secret {
                            secrets::load_secret(&sid, secrets::SecretKind::OidcClientSecret)
                                .await
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                        let result = auth::oidc_login(
                            &issuer,
                            &client,
                            secret.as_deref().map(String::as_str),
                            &scopes,
                        )
                        .await
                        .map(|t| (sid, t.access_token, t.refresh_token));
                        Message::OidcResult(result)
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
            }

            Message::OidcResult(result) => match result {
                Ok((source_id, access_token, refresh_token)) => {
                    if let Some(src) = self
                        .calendar_config
                        .sources
                        .iter_mut()
                        .find(|s| s.id == source_id)
                    {
                        let auth = match &mut src.source_type {
                            SourceType::CalDav { auth, .. } => Some(auth),
                            SourceType::IcsUrl { auth, .. } => Some(auth),
                            _ => None,
                        };
                        if let Some(AuthMethod::Oidc { has_token, .. }) = auth {
                            *has_token = true;
                        }
                    }
                    self.save_config();
                    self.refresh_attempted.remove(&source_id);
                    self.discovery_error = None;

                    // Store tokens then discover via keyring roundtrip
                    let sid = source_id;
                    return cosmic::Task::future(async move {
                        let access_token = Zeroizing::new(access_token);
                        let refresh_token = refresh_token.map(Zeroizing::new);
                        if let Err(e) = secrets::store_secret(
                            &sid,
                            secrets::SecretKind::OidcAccessToken,
                            &access_token,
                        )
                        .await
                        {
                            error!("Failed to store OIDC access token: {e}");
                        }
                        if let Some(ref rt) = refresh_token {
                            if let Err(e) = secrets::store_secret(
                                &sid,
                                secrets::SecretKind::OidcRefreshToken,
                                rt,
                            )
                            .await
                            {
                                error!("Failed to store OIDC refresh token: {e}");
                            }
                        }
                        Message::Discover(sid)
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
                Err(e) => {
                    error!("OIDC login failed: {e}");
                }
            },

            // ── OIDC token refresh (automatic on auth-expired) ──
            Message::OidcRefresh(source_id) => {
                let oidc_info = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                    .and_then(|s| match &s.source_type {
                        SourceType::CalDav {
                            auth: AuthMethod::Oidc { issuer_url, client_id, has_client_secret, .. }, ..
                        }
                        | SourceType::IcsUrl {
                            auth: AuthMethod::Oidc { issuer_url, client_id, has_client_secret, .. }, ..
                        } => Some((issuer_url.clone(), client_id.clone(), *has_client_secret)),
                        _ => None,
                    });

                if let Some((issuer, client_id, needs_secret)) = oidc_info {
                    let sid = source_id;
                    return cosmic::Task::future(async move {
                        // Load refresh token from keyring
                        let refresh_token = secrets::load_secret(&sid, secrets::SecretKind::OidcRefreshToken)
                            .await
                            .ok()
                            .flatten();
                        let Some(rt) = refresh_token else {
                            return Message::OidcRefreshResult(Err(format!(
                                "No refresh token stored for {sid}"
                            )));
                        };
                        let secret = if needs_secret {
                            secrets::load_secret(&sid, secrets::SecretKind::OidcClientSecret)
                                .await
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                        match auth::oidc_refresh(&issuer, &client_id, secret.as_deref().map(String::as_str), &rt).await {
                            Ok(tokens) => Message::OidcRefreshResult(Ok((
                                sid,
                                tokens.access_token,
                                tokens.refresh_token,
                            ))),
                            Err(e) => Message::OidcRefreshResult(Err(format!("{sid}: {e}"))),
                        }
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
            }

            Message::OidcRefreshResult(result) => match result {
                Ok((source_id, access_token, refresh_token)) => {
                    tracing::info!("Token refresh succeeded for {source_id}");
                    self.discovery_error = None;

                    // Store tokens then discover via keyring roundtrip
                    let sid = source_id;
                    return cosmic::Task::future(async move {
                        let access_token = Zeroizing::new(access_token);
                        let refresh_token = refresh_token.map(Zeroizing::new);
                        if let Err(e) = secrets::store_secret(
                            &sid,
                            secrets::SecretKind::OidcAccessToken,
                            &access_token,
                        )
                        .await
                        {
                            error!("Failed to store refreshed access token: {e}");
                        }
                        if let Some(ref rt) = refresh_token {
                            if let Err(e) = secrets::store_secret(
                                &sid,
                                secrets::SecretKind::OidcRefreshToken,
                                rt,
                            )
                            .await
                            {
                                error!("Failed to store refreshed refresh token: {e}");
                            }
                        }
                        Message::Discover(sid)
                    })
                    .map(crate::pages::Message::Calendar)
                    .map(crate::Message::PageMessage);
                }
                Err(e) => {
                    error!("Token refresh failed, full re-auth needed: {e}");
                    // Extract source_id from error message prefix
                    let source_id = e.split(':').next().unwrap_or("").trim().to_string();
                    if !source_id.is_empty() {
                        self.discovery_error = Some(fl!("calendar-auth-expired-reauth"));
                        // Trigger full re-auth
                        return self.update(Message::OidcLogin(source_id));
                    }
                    self.discovery_error = Some(e);
                }
            },

            // ── Sync settings ──────────────────────────────────
            Message::SyncInterval(v) => {
                self.calendar_config.sync_interval_minutes = v;
                self.save_config();
            }

            Message::UpcomingCount(v) => {
                self.calendar_config.upcoming_count = v;
                self.save_config();
            }

            Message::EncryptionModeChanged(v) => {
                let new_mode = match v {
                    0 => EncryptionMode::None,
                    1 => EncryptionMode::Auto,
                    _ => EncryptionMode::Manual,
                };
                if self.calendar_config.encryption_mode != new_mode {
                    self.calendar_config.encryption_mode = new_mode;
                    self.save_config();
                    // Delete old cache files so next sync rebuilds with new mode
                    delete_calendar_cache();
                }
            }



            Message::ConfigReload => {
                if let Some(ref handle) = self.config_handle {
                    if let Ok(config) = CalendarConfig::get_entry(handle) {
                        self.calendar_config = config;
                    }
                }
            }
        }

        Task::none()
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    // Source management
    AddSource,
    EditSource(String),
    RemoveSource(String),
    ToggleSource(String),
    SaveSource,
    SaveEditSource,
    CancelForm,
    // Form fields
    FormName(String),
    FormUrl(String),
    FormSourceType(usize),
    FormAuth(usize),
    FormUsername(String),
    FormPassword(String),
    FormToken(String),
    FormColor(String),
    FormOidcIssuer(String),
    FormOidcClientId(String),
    FormOidcClientSecret(String),
    FormOidcScopes(String),
    FormCaCertPath(String),
    BrowseCaCert,
    CaCertBrowsed(Arc<Result<url::Url, String>>),
    // Form-level discovery (two-stage CalDAV add)
    FormDiscover,
    FormDiscovered(Vec<CalDavCalendar>),
    FormDiscoverError(String),
    FormToggleCalendar(usize),
    FormCalendarColor(usize),
    FormCalColorInput(String),
    FormCalColorPreset(String),
    FormCalColorSave,
    FormCalColorCancel,
    FormOidcLogin,
    FormOidcLoginResult(Result<(String, Option<String>), String>),
    /// After OIDC login saved the source: enter edit view + discover calendars.
    /// Carries (source_id, access_token) so discovery can use inline credentials.
    OidcSavedAndDiscover(String, String),
    // Discovery (for already-saved sources)
    Discover(String),
    Discovered(String, Vec<CalDavCalendar>),
    DiscoverError(String, String),
    ToggleCalendar(String, String),
    // Per-calendar color
    EditCalendarColor(String, String),
    CalendarColorInput(String),
    CalendarColorPreset(String),
    SaveCalendarColor,
    CancelCalendarColor,
    // OIDC
    OidcLogin(String),
    OidcResult(Result<(String, String, Option<String>), String>),
    /// Attempt to refresh OIDC tokens for a source, then retry discovery.
    OidcRefresh(String),
    /// Result of token refresh: Ok((source_id, access_token, refresh_token)) or Err.
    OidcRefreshResult(Result<(String, String, Option<String>), String>),
    // Sync settings
    SyncInterval(u64),
    UpcomingCount(usize),
    EncryptionModeChanged(usize),
    // Config reload
    ConfigReload,
}

// ── Sections ───────────────────────────────────────────────────────

fn sources_section() -> Section<crate::pages::Message> {
    crate::slab!(descriptions {
        title = fl!("calendar-sources");
    });

    Section::default()
        .title(fl!("calendar-sources"))
        .descriptions(descriptions)
        .view::<Page>(move |_binder, page, section| {
            // Hide when editing — the form section takes the full page
            if page.editing {
                return widget::column::with_capacity(0)
                    .apply(cosmic::Element::from)
                    .map(crate::pages::Message::Calendar);
            }

            let mut section_content = settings::section().title(&section.title);

            if page.calendar_config.sources.is_empty() {
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-no-sources"))
                        .control(
                            widget::button::standard(fl!("calendar-add-source"))
                                .on_press(Message::AddSource),
                        ),
                );
            } else {
                for source in &page.calendar_config.sources {
                    let source_id = source.id.clone();
                    let type_label = match &source.source_type {
                        SourceType::CalDav { .. } => fl!("calendar-source-type-caldav"),
                        SourceType::IcsUrl { .. } => fl!("calendar-source-type-ics-url"),
                        SourceType::IcsFile { .. } => fl!("calendar-source-type-ics-file"),
                    };

                    // Parse source color for the indicator (ICS sources only)
                    let is_caldav = matches!(&source.source_type, SourceType::CalDav { .. });

                    let desc = format!("{type_label}");
                    let toggle_id = source_id.clone();
                    let edit_id = source_id.clone();
                    let remove_id = source_id.clone();

                    // Check if this source uses OIDC auth
                    let is_oidc = matches!(&source.source_type,
                        SourceType::CalDav { auth: AuthMethod::Oidc { .. }, .. }
                        | SourceType::IcsUrl { auth: AuthMethod::Oidc { .. }, .. }
                    );

                    let mut controls = widget::row::with_capacity(5)
                        .spacing(8);

                    if !is_caldav {
                        let color = parse_hex_color(&source.color);
                        let color_indicator = widget::container(widget::Space::new().width(12).height(12))
                            .class(color_swatch_class(color));
                        controls = controls.push(color_indicator);
                    }

                    // Re-auth button for OIDC sources
                    if is_oidc {
                        let reauth_id = source_id.clone();
                        controls = controls.push(
                            widget::button::icon(widget::icon::from_name("system-lock-screen-symbolic"))
                                .on_press(Message::OidcLogin(reauth_id)),
                        );
                    }

                    // Discover button for CalDAV sources without calendars
                    if let SourceType::CalDav { calendars, .. } = &source.source_type {
                        if calendars.is_empty() {
                            let discover_id = source_id.clone();
                            controls = controls.push(
                                widget::button::icon(widget::icon::from_name("view-refresh-symbolic"))
                                    .on_press(Message::Discover(discover_id)),
                            );
                        }
                    }

                    controls = controls
                        .push(
                            widget::toggler(source.enabled)
                                .on_toggle(move |_| Message::ToggleSource(toggle_id.clone())),
                        )
                        .push(
                            widget::button::icon(widget::icon::from_name("edit-symbolic"))
                            .on_press(Message::EditSource(edit_id)),
                        )
                        .push(
                            widget::button::icon(widget::icon::from_name("edit-delete-symbolic"))
                            .on_press(Message::RemoveSource(remove_id)),
                        );

                    section_content = section_content.add(
                        settings::item::builder(&source.name)
                            .description(desc)
                            .control(controls),
                    );

                    // Show CalDAV calendars
                    if let SourceType::CalDav { calendars, .. } = &source.source_type {
                        for cal in calendars {
                            let cal_color = if cal.color.is_empty() {
                                &source.color
                            } else {
                                &cal.color
                            };
                            let cal_color_parsed = parse_hex_color(cal_color);
                            let cal_indicator =
                                widget::container(widget::Space::new().width(10).height(10))
                                    .class(color_swatch_class(cal_color_parsed));

                            let src_id = source.id.clone();
                            let cal_href = cal.href.clone();
                            let color_src_id = source.id.clone();
                            let color_cal_href = cal.href.clone();

                            let cal_controls = widget::row::with_capacity(3)
                                .spacing(8)
                                .push(cal_indicator)
                                .push(
                                    widget::toggler(cal.enabled).on_toggle(move |_| {
                                        Message::ToggleCalendar(
                                            src_id.clone(),
                                            cal_href.clone(),
                                        )
                                    }),
                                )
                                .push(
                                    widget::button::icon(widget::icon::from_name("preferences-color-symbolic"))
                                    .on_press(Message::EditCalendarColor(
                                        color_src_id,
                                        color_cal_href,
                                    )),
                                );

                            section_content = section_content.add(
                                settings::item::builder(format!("    {}", cal.display_name))
                                    .control(cal_controls),
                            );

                            // Per-calendar color editing inline
                            if let Some((ref edit_sid, ref edit_href)) =
                                page.editing_calendar_color
                            {
                                if edit_sid == &source.id && edit_href == &cal.href {
                                    let mut color_row =
                                        widget::row::with_capacity(COLOR_PRESETS.len() + 2)
                                            .spacing(4);

                                    for &hex in COLOR_PRESETS {
                                        let hex_color = parse_hex_color(hex);
                                        let swatch = widget::container(
                                            widget::Space::new().width(20).height(20),
                                        )
                                        .class(color_swatch_class(hex_color))
                                        .apply(widget::button::custom)
                                        .class(if page.calendar_color_input == hex {
                                            cosmic::theme::Button::Suggested
                                        } else {
                                            cosmic::theme::Button::Standard
                                        })
                                        .padding(2)
                                        .on_press(Message::CalendarColorPreset(hex.to_string()));

                                        color_row = color_row.push(swatch);
                                    }

                                    let input = widget::text_input(
                                        fl!("calendar-source-color"),
                                        &page.calendar_color_input,
                                    )
                                    .on_input(Message::CalendarColorInput)
                                    .width(Length::Fixed(100.0));

                                    let save_btn = widget::button::standard(fl!("calendar-save"))
                                        .on_press(Message::SaveCalendarColor);
                                    let cancel_btn =
                                        widget::button::standard(fl!("calendar-cancel"))
                                            .on_press(Message::CancelCalendarColor);

                                    let controls_row = widget::row::with_capacity(3)
                                        .spacing(8)
                                        .push(input)
                                        .push(save_btn)
                                        .push(cancel_btn);

                                    section_content = section_content.add(
                                        settings::item::builder("").control(
                                            widget::column::with_capacity(2)
                                                .spacing(4)
                                                .push(color_row)
                                                .push(controls_row),
                                        ),
                                    );
                                }
                            }
                        }

                        // Discover button
                        let is_discovering = page
                            .discovering_source
                            .as_ref()
                            .map_or(false, |id| id == &source.id);
                        let discover_label = if is_discovering {
                            fl!("calendar-discovering")
                        } else {
                            fl!("calendar-discover")
                        };
                        let discover_src_id = source.id.clone();
                        let mut discover_btn =
                            widget::button::standard(discover_label);
                        if !is_discovering {
                            discover_btn =
                                discover_btn.on_press(Message::Discover(discover_src_id));
                        }
                        section_content = section_content.add(
                            settings::item::builder("").control(discover_btn),
                        );
                    }

                    // OIDC login button
                    if let SourceType::CalDav {
                        auth: AuthMethod::Oidc { has_token, .. },
                        ..
                    }
                    | SourceType::IcsUrl {
                        auth: AuthMethod::Oidc { has_token, .. },
                        ..
                    } = &source.source_type
                    {
                        if !has_token {
                            let login_id = source.id.clone();
                            section_content = section_content.add(
                                settings::item::builder("").control(
                                    widget::button::suggested(fl!("calendar-source-oidc-login"))
                                        .on_press(Message::OidcLogin(login_id)),
                                ),
                            );
                        }
                    }
                }

                // Add source button (when list is non-empty)
                if !page.editing {
                    section_content = section_content.add(
                        settings::item::builder("").control(
                            widget::button::standard(fl!("calendar-add-source"))
                                .on_press(Message::AddSource),
                        ),
                    );
                }
            }

            // Discovery error
            if let Some(ref err) = page.discovery_error {
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-discover-error"))
                        .description(err.clone())
                        .control(space::horizontal()),
                );
            }

            section_content
                .apply(cosmic::Element::from)
                .map(crate::pages::Message::Calendar)
        })
}

fn source_form_section() -> Section<crate::pages::Message> {
    crate::slab!(descriptions {
        title = fl!("calendar-add-source");
    });

    Section::default()
        .title(fl!("calendar-add-source"))
        .descriptions(descriptions)
        .view::<Page>(move |_binder, page, section| {
            if !page.editing {
                // Empty section when not editing
                return widget::column::with_capacity(0)
                    .apply(cosmic::Element::from)
                    .map(crate::pages::Message::Calendar);
            }

            let is_edit = page.editing_source_id.is_some();
            let title = if is_edit {
                fl!("calendar-edit-source")
            } else {
                section.title.clone()
            };

            let mut section_content = settings::section().title(title);

            // Name
            section_content = section_content.add(
                settings::item::builder(fl!("calendar-source-name")).control(
                    widget::text_input(fl!("calendar-source-name"), &page.form_name)
                        .on_input(Message::FormName)
                        .width(Length::Fixed(300.0)),
                ),
            );

            // Source type
            let type_labels = [
                fl!("calendar-source-type-caldav"),
                fl!("calendar-source-type-ics-url"),
                fl!("calendar-source-type-ics-file"),
            ];
            let mut type_row = widget::row::with_capacity(3).spacing(4);
            for (i, label) in type_labels.iter().enumerate() {
                let btn = if i == page.form_source_type {
                    widget::button::suggested(label.clone())
                } else {
                    widget::button::standard(label.clone())
                }
                .on_press(Message::FormSourceType(i));
                type_row = type_row.push(btn);
            }
            section_content = section_content.add(
                settings::item::builder(fl!("calendar-source-type")).control(type_row),
            );

            // URL / Path
            let url_placeholder = if page.form_source_type == 2 {
                fl!("calendar-source-path")
            } else {
                fl!("calendar-source-url")
            };
            let url_item_label = if page.form_source_type == 2 {
                fl!("calendar-source-path")
            } else {
                fl!("calendar-source-url")
            };
            section_content = section_content.add(
                settings::item::builder(url_item_label).control(
                    widget::text_input(url_placeholder, &page.form_url)
                        .on_input(Message::FormUrl)
                        .width(Length::Fixed(400.0)),
                ),
            );

            // Validation errors
            if !page.form_name.is_empty() && page.form_name.trim().is_empty() {
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-validation-name-required"))
                        .control(space::horizontal()),
                );
            }
            if !page.form_url.is_empty() {
                let url_valid = match page.form_source_type {
                    0 | 1 => {
                        let u = page.form_url.trim();
                        u.starts_with("http://") || u.starts_with("https://")
                    }
                    2 => {
                        let p = page.form_url.trim();
                        !p.is_empty() && std::path::Path::new(p).exists()
                    }
                    _ => true,
                };
                if !url_valid {
                    let err = if page.form_source_type == 2 {
                        fl!("calendar-validation-file-not-found")
                    } else {
                        fl!("calendar-validation-url-invalid")
                    };
                    section_content = section_content.add(
                        settings::item::builder(err)
                            .control(space::horizontal()),
                    );
                }
            }

            // Auth selector (CalDAV and ICS URL)
            if page.form_source_type == 0 || page.form_source_type == 1 {
                let auth_labels = [
                    fl!("calendar-source-auth-none"),
                    fl!("calendar-source-auth-basic"),
                    fl!("calendar-source-auth-bearer"),
                    fl!("calendar-source-auth-oidc"),
                ];
                let mut auth_row = widget::row::with_capacity(4).spacing(4);
                for (i, label) in auth_labels.iter().enumerate() {
                    let btn = if i == page.form_auth_method {
                        widget::button::suggested(label.clone())
                    } else {
                        widget::button::standard(label.clone())
                    }
                    .on_press(Message::FormAuth(i));
                    auth_row = auth_row.push(btn);
                }
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-source-auth")).control(auth_row),
                );

                match page.form_auth_method {
                    1 => {
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-username")).control(
                                widget::text_input(
                                    fl!("calendar-source-username"),
                                    &page.form_username,
                                )
                                .on_input(Message::FormUsername)
                                .width(Length::Fixed(300.0)),
                            ),
                        );
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-password")).control(
                                widget::text_input(
                                    fl!("calendar-source-password"),
                                    &page.form_password,
                                )
                                .on_input(Message::FormPassword)
                                .password()
                                .width(Length::Fixed(300.0)),
                            ),
                        );
                    }
                    2 => {
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-token")).control(
                                widget::text_input(
                                    fl!("calendar-source-token"),
                                    &page.form_token,
                                )
                                .on_input(Message::FormToken)
                                .password()
                                .width(Length::Fixed(300.0)),
                            ),
                        );
                    }
                    3 => {
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-oidc-issuer")).control(
                                widget::text_input(
                                    fl!("calendar-source-oidc-issuer"),
                                    &page.form_oidc_issuer,
                                )
                                .on_input(Message::FormOidcIssuer)
                                .width(Length::Fixed(400.0)),
                            ),
                        );
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-oidc-client-id")).control(
                                widget::text_input(
                                    fl!("calendar-source-oidc-client-id"),
                                    &page.form_oidc_client_id,
                                )
                                .on_input(Message::FormOidcClientId)
                                .width(Length::Fixed(300.0)),
                            ),
                        );
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-oidc-client-secret"))
                                .control(
                                    widget::text_input(
                                        fl!("calendar-source-oidc-client-secret-hint"),
                                        &page.form_oidc_client_secret,
                                    )
                                    .on_input(Message::FormOidcClientSecret)
                                    .password()
                                    .width(Length::Fixed(300.0)),
                                ),
                        );
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-oidc-scopes")).control(
                                widget::text_input(
                                    fl!("calendar-source-oidc-scopes"),
                                    &page.form_oidc_scopes,
                                )
                                .on_input(Message::FormOidcScopes)
                                .width(Length::Fixed(300.0)),
                            ),
                        );
                        section_content = section_content.add(
                            settings::item::builder(fl!("calendar-source-oidc-callback-url"))
                                .control(
                                    widget::text::body(fl!("calendar-source-oidc-callback-pattern")),
                                ),
                        );
                    }
                    _ => {}
                }

                // CA Certificate
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-source-ca-cert")).control(
                        widget::row::with_capacity(2)
                            .spacing(8)
                            .push(
                                widget::text_input(
                                    fl!("calendar-source-ca-cert"),
                                    &page.form_ca_cert_path,
                                )
                                .on_input(Message::FormCaCertPath)
                                .width(Length::Fixed(350.0)),
                            )
                            .push(
                                widget::button::standard(fl!("calendar-browse"))
                                    .on_press(Message::BrowseCaCert),
                            ),
                    ),
                );
            }

            // Color picker for source (only for ICS sources; CalDAV uses per-calendar colors)
            if page.form_source_type != 0 {
                let mut color_row = widget::row::with_capacity(COLOR_PRESETS.len()).spacing(4);
                for &hex in COLOR_PRESETS {
                    let hex_color = parse_hex_color(hex);
                    let swatch = widget::container(widget::Space::new().width(20).height(20))
                        .class(color_swatch_class(hex_color))
                        .apply(widget::button::custom)
                        .class(if page.form_color.eq_ignore_ascii_case(hex) {
                            cosmic::theme::Button::Suggested
                        } else {
                            cosmic::theme::Button::Standard
                        })
                        .padding(2)
                        .on_press(Message::FormColor(hex.to_string()));
                    color_row = color_row.push(swatch);
                }
                section_content = section_content.add(
                    settings::item::builder(fl!("calendar-source-color")).control(
                        widget::column::with_capacity(2)
                            .spacing(4)
                            .push(color_row)
                            .push(
                                widget::text_input(fl!("calendar-source-color"), &page.form_color)
                                    .on_input(Message::FormColor)
                                    .width(Length::Fixed(120.0)),
                            ),
                    ),
                );
            }
            // ── CalDAV two-stage: discover then configure calendars ──
            if page.form_source_type == 0 && !is_edit {
                // Discovery error
                if let Some(ref err) = page.form_discovery_error {
                    section_content = section_content.add(
                        settings::item::builder("").control(
                            widget::text::body(err.clone())
                                .apply(widget::container)
                                .padding(4),
                        ),
                    );
                }

                // Discover / OIDC Login buttons
                if page.form_discovered_calendars.is_empty() {
                    let mut action_row = widget::row::with_capacity(3).spacing(8);

                    if page.form_auth_method == 3 && !page.form_oidc_logged_in {
                        // OIDC: show login button first
                        let mut login_btn = widget::button::suggested(
                            fl!("calendar-source-oidc-login"),
                        );
                        if page.form_valid() {
                            login_btn = login_btn.on_press(Message::FormOidcLogin);
                        }
                        action_row = action_row.push(login_btn);
                    } else {
                        // Non-OIDC or already logged in: show discover button
                        let discover_label = if page.form_discovering {
                            fl!("calendar-discovering")
                        } else {
                            fl!("calendar-discover")
                        };
                        let mut discover_btn =
                            widget::button::suggested(discover_label);
                        if page.form_valid() && !page.form_discovering {
                            discover_btn = discover_btn.on_press(Message::FormDiscover);
                        }
                        action_row = action_row.push(discover_btn);
                    }

                    action_row = action_row.push(
                        widget::button::standard(fl!("calendar-cancel"))
                            .on_press(Message::CancelForm),
                    );

                    section_content = section_content.add(
                        settings::item::builder("").control(action_row),
                    );
                } else {
                    // Show discovered calendars with toggles and color pickers
                    for (idx, cal) in page.form_discovered_calendars.iter().enumerate() {
                        let cal_color = if cal.color.is_empty() {
                            "#1a73e8"
                        } else {
                            &cal.color
                        };
                        let cal_color_parsed = parse_hex_color(cal_color);
                        let cal_indicator =
                            widget::container(widget::Space::new().width(10).height(10))
                                .class(color_swatch_class(cal_color_parsed));

                        let cal_controls = widget::row::with_capacity(3)
                            .spacing(8)
                            .push(cal_indicator)
                            .push(
                                widget::toggler(cal.enabled)
                                    .on_toggle(move |_| Message::FormToggleCalendar(idx)),
                            )
                            .push(
                                widget::button::icon(widget::icon::from_name(
                                    "preferences-color-symbolic",
                                ))
                                .on_press(Message::FormCalendarColor(idx)),
                            );

                        section_content = section_content.add(
                            settings::item::builder(&cal.display_name)
                                .control(cal_controls),
                        );

                        // Inline color editor for this calendar
                        if page.form_editing_cal_color == Some(idx) {
                            let mut color_row =
                                widget::row::with_capacity(COLOR_PRESETS.len()).spacing(4);
                            for &hex in COLOR_PRESETS {
                                let hex_color = parse_hex_color(hex);
                                let swatch = widget::container(
                                    widget::Space::new().width(20).height(20),
                                )
                                .class(color_swatch_class(hex_color))
                                .apply(widget::button::custom)
                                .class(
                                    if page.form_cal_color_input.eq_ignore_ascii_case(hex) {
                                        cosmic::theme::Button::Suggested
                                    } else {
                                        cosmic::theme::Button::Standard
                                    },
                                )
                                .padding(2)
                                .on_press(Message::FormCalColorPreset(hex.to_string()));
                                color_row = color_row.push(swatch);
                            }

                            let controls_row = widget::row::with_capacity(3)
                                .spacing(8)
                                .push(
                                    widget::text_input(
                                        fl!("calendar-source-color"),
                                        &page.form_cal_color_input,
                                    )
                                    .on_input(Message::FormCalColorInput)
                                    .width(Length::Fixed(100.0)),
                                )
                                .push(
                                    widget::button::standard(fl!("calendar-save"))
                                        .on_press(Message::FormCalColorSave),
                                )
                                .push(
                                    widget::button::standard(fl!("calendar-cancel"))
                                        .on_press(Message::FormCalColorCancel),
                                );

                            section_content = section_content.add(
                                settings::item::builder("").control(
                                    widget::column::with_capacity(2)
                                        .spacing(4)
                                        .push(color_row)
                                        .push(controls_row),
                                ),
                            );
                        }
                    }

                    // Re-discover button
                    let mut rediscover_btn = widget::button::standard(fl!("calendar-discover"));
                    if !page.form_discovering {
                        rediscover_btn = rediscover_btn.on_press(Message::FormDiscover);
                    }

                    // Save / Cancel
                    let has_enabled = page
                        .form_discovered_calendars
                        .iter()
                        .any(|c| c.enabled);
                    let mut save_btn = widget::button::suggested(fl!("calendar-save"));
                    if has_enabled {
                        save_btn = save_btn.on_press(Message::SaveSource);
                    }

                    section_content = section_content.add(
                        settings::item::builder("").control(
                            widget::row::with_capacity(3)
                                .spacing(8)
                                .push(save_btn)
                                .push(rediscover_btn)
                                .push(
                                    widget::button::standard(fl!("calendar-cancel"))
                                        .on_press(Message::CancelForm),
                                ),
                        ),
                    );
                }
            } else {
                // ── Edit mode (CalDAV): show calendars + discover ──
                if is_edit && page.form_source_type == 0 {
                    // Show existing calendars from the source being edited
                    if let Some(src) = page
                        .calendar_config
                        .sources
                        .iter()
                        .find(|s| s.id.as_str() == page.editing_source_id.as_deref().unwrap_or(""))
                    {
                        if let SourceType::CalDav { calendars, .. } = &src.source_type {
                            if !calendars.is_empty() {
                                section_content = section_content.add(
                                    settings::item::builder(fl!("calendar-sources"))
                                        .control(space::horizontal()),
                                );
                            }
                            for cal in calendars {
                                let cal_color = if cal.color.is_empty() {
                                    &src.color
                                } else {
                                    &cal.color
                                };
                                let cal_color_parsed = parse_hex_color(cal_color);
                                let cal_indicator =
                                    widget::container(widget::Space::new().width(10).height(10))
                                        .class(color_swatch_class(cal_color_parsed));

                                let src_id = src.id.clone();
                                let cal_href = cal.href.clone();
                                let color_src_id = src.id.clone();
                                let color_cal_href = cal.href.clone();

                                let cal_controls = widget::row::with_capacity(3)
                                    .spacing(8)
                                    .push(cal_indicator)
                                    .push(
                                        widget::toggler(cal.enabled).on_toggle(move |_| {
                                            Message::ToggleCalendar(
                                                src_id.clone(),
                                                cal_href.clone(),
                                            )
                                        }),
                                    )
                                    .push(
                                        widget::button::icon(widget::icon::from_name(
                                            "preferences-color-symbolic",
                                        ))
                                        .on_press(Message::EditCalendarColor(
                                            color_src_id,
                                            color_cal_href,
                                        )),
                                    );

                                section_content = section_content.add(
                                    settings::item::builder(&cal.display_name)
                                        .control(cal_controls),
                                );

                                // Per-calendar color editing inline
                                if let Some((ref edit_sid, ref edit_href)) =
                                    page.editing_calendar_color
                                {
                                    if edit_sid == &src.id && edit_href == &cal.href {
                                        let mut color_row =
                                            widget::row::with_capacity(COLOR_PRESETS.len() + 2)
                                                .spacing(4);

                                        for &hex in COLOR_PRESETS {
                                            let hex_color = parse_hex_color(hex);
                                            let swatch = widget::container(
                                                widget::Space::new().width(20).height(20),
                                            )
                                            .class(color_swatch_class(hex_color))
                                            .apply(widget::button::custom)
                                            .class(if page.calendar_color_input == hex {
                                                cosmic::theme::Button::Suggested
                                            } else {
                                                cosmic::theme::Button::Standard
                                            })
                                            .padding(2)
                                            .on_press(Message::CalendarColorPreset(hex.to_string()));

                                            color_row = color_row.push(swatch);
                                        }

                                        let input = widget::text_input(
                                            fl!("calendar-source-color"),
                                            &page.calendar_color_input,
                                        )
                                        .on_input(Message::CalendarColorInput)
                                        .width(Length::Fixed(100.0));

                                        let save_btn = widget::button::standard(fl!("calendar-save"))
                                            .on_press(Message::SaveCalendarColor);
                                        let cancel_btn =
                                            widget::button::standard(fl!("calendar-cancel"))
                                                .on_press(Message::CancelCalendarColor);

                                        let controls_row = widget::row::with_capacity(3)
                                            .spacing(8)
                                            .push(input)
                                            .push(save_btn)
                                            .push(cancel_btn);

                                        section_content = section_content.add(
                                            settings::item::builder("").control(
                                                widget::column::with_capacity(2)
                                                    .spacing(4)
                                                    .push(color_row)
                                                    .push(controls_row),
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // Discover button
                    let edit_id = page.editing_source_id.clone().unwrap_or_default();
                    let is_discovering =
                        page.discovering_source.as_deref() == Some(edit_id.as_str());
                    let discover_label = if is_discovering {
                        fl!("calendar-discovering")
                    } else {
                        fl!("calendar-discover")
                    };
                    let mut discover_btn = widget::button::standard(discover_label);
                    if !is_discovering && page.form_valid() {
                        discover_btn = discover_btn.on_press(Message::Discover(edit_id));
                    }

                    // Save / Cancel / Discover
                    let mut save_btn = widget::button::suggested(fl!("calendar-save"));
                    if page.form_valid() {
                        save_btn = save_btn.on_press(Message::SaveEditSource);
                    }
                    section_content = section_content.add(
                        settings::item::builder("").control(
                            widget::row::with_capacity(3)
                                .spacing(8)
                                .push(save_btn)
                                .push(discover_btn)
                                .push(
                                    widget::button::standard(fl!("calendar-cancel"))
                                        .on_press(Message::CancelForm),
                                ),
                        ),
                    );
                } else {
                    // ICS sources or new ICS source: simple Save / Cancel
                    let save_msg = if is_edit {
                        Message::SaveEditSource
                    } else {
                        Message::SaveSource
                    };
                    let mut save_btn = widget::button::suggested(fl!("calendar-save"));
                    if page.form_valid() {
                        save_btn = save_btn.on_press(save_msg);
                    }
                    section_content = section_content.add(
                        settings::item::builder("").control(
                            widget::row::with_capacity(2)
                                .spacing(8)
                                .push(save_btn)
                                .push(
                                    widget::button::standard(fl!("calendar-cancel"))
                                        .on_press(Message::CancelForm),
                                ),
                        ),
                    );
                }
            }

            section_content
                .apply(cosmic::Element::from)
                .map(crate::pages::Message::Calendar)
        })
}

fn sync_settings_section() -> Section<crate::pages::Message> {
    crate::slab!(descriptions {
        sync_interval = fl!("calendar-sync-interval");
        upcoming_count = fl!("calendar-upcoming-count");
    });

    Section::default()
        .title(fl!("calendar-sync-settings"))
        .descriptions(descriptions)
        .view::<Page>(move |_binder, page, section| {
            // Hide when editing — the form section takes the full page
            if page.editing {
                return widget::column::with_capacity(0)
                    .apply(cosmic::Element::from)
                    .map(crate::pages::Message::Calendar);
            }

            let mut section_content = settings::section().title(&section.title);

            // Sync interval - use spin_button for numeric values
            section_content = section_content.add(
                settings::item::builder(&*section.descriptions[sync_interval]).control(
                    widget::spin_button(
                        page.calendar_config.sync_interval_minutes.to_string(),
                        "sync interval",
                        page.calendar_config.sync_interval_minutes,
                        1,
                        1,
                        1440,
                        Message::SyncInterval,
                    ),
                ),
            );

            // Upcoming count
            section_content = section_content.add(
                settings::item::builder(&*section.descriptions[upcoming_count]).control(
                    widget::spin_button(
                        page.calendar_config.upcoming_count.to_string(),
                        "upcoming count",
                        page.calendar_config.upcoming_count,
                        1,
                        1,
                        50,
                        |v| Message::UpcomingCount(v as usize),
                    ),
                ),
            );

            // Encryption mode selector
            let current_mode = match page.calendar_config.encryption_mode {
                EncryptionMode::None => 0,
                EncryptionMode::Auto => 1,
                EncryptionMode::Manual => 2,
            };
            let mode_labels = [
                fl!("calendar-encryption-none"),
                fl!("calendar-encryption-auto"),
                fl!("calendar-encryption-manual"),
            ];
            let mut mode_row = widget::row::with_capacity(3).spacing(4);
            for (i, label) in mode_labels.iter().enumerate() {
                let btn = if i == current_mode {
                    widget::button::suggested(label.clone())
                } else {
                    widget::button::standard(label.clone())
                }
                .on_press(Message::EncryptionModeChanged(i));
                mode_row = mode_row.push(btn);
            }
            section_content = section_content.add(
                settings::item::builder(fl!("calendar-encryption-mode")).control(mode_row),
            );

            section_content
                .apply(cosmic::Element::from)
                .map(crate::pages::Message::Calendar)
        })
}

// ── Cache cleanup ──────────────────────────────────────────────────

/// Delete calendar cache files so the next sync rebuilds them.
fn delete_calendar_cache() {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".config"))
        });
    if let Some(dir) = config_dir {
        let base = dir
            .join("cosmic")
            .join("com.system76.CosmicAppletTime.Calendar")
            .join("v1");
        let _ = std::fs::remove_file(base.join("event_cache.json"));
        let _ = std::fs::remove_file(base.join("ctag_cache.json"));
    }
}

// ── Color swatch container style ───────────────────────────────────

fn parse_hex_color(hex: &str) -> cosmic::iced::Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(128);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(128);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(128);
        cosmic::iced::Color::from_rgb8(r, g, b)
    } else {
        cosmic::iced::Color::from_rgb8(128, 128, 128)
    }
}

fn color_swatch_class(color: cosmic::iced::Color) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::Custom(Box::new(move |_theme| {
        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(cosmic::iced::Background::Color(color)),
            border: cosmic::iced_core::Border {
                radius: 4.0.into(),
                width: 1.0,
                color: cosmic::iced::Color::from_rgba8(0, 0, 0, 0.3),
            },
            shadow: Default::default(),
            snap: false,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_page() -> Page {
        Page {
            entity: page::Entity::default(),
            calendar_config: CalendarConfig::default(),
            config_handle: None,
            editing: false,
            editing_source_id: None,
            form_name: String::new(),
            form_url: String::new(),
            form_source_type: 0,
            form_auth_method: 0,
            form_username: String::new(),
            form_password: String::new(),
            form_token: String::new(),
            form_color: "#1a73e8".into(),
            form_oidc_issuer: String::new(),
            form_oidc_client_id: String::new(),
            form_oidc_client_secret: String::new(),
            form_oidc_scopes: "openid".into(),
            form_ca_cert_path: String::new(),
            form_discovered_calendars: Vec::new(),
            form_discovering: false,
            form_discovery_error: None,
            form_source_id: "test-form".into(),
            form_oidc_logged_in: false,
            form_editing_cal_color: None,
            form_cal_color_input: String::new(),
            discovering_source: None,
            discovery_error: None,
            refresh_attempted: std::collections::HashSet::new(),
            editing_calendar_color: None,
            calendar_color_input: String::new(),
        }
    }

    #[test]
    fn form_valid_empty_name() {
        let mut p = test_page();
        p.form_name = "".into();
        p.form_url = "https://dav.example.com".into();
        p.form_source_type = 0;
        assert!(!p.form_valid());
    }

    #[test]
    fn form_valid_caldav_invalid_url() {
        let mut p = test_page();
        p.form_name = "My Calendar".into();
        p.form_url = "ftp://bad-scheme.com".into();
        p.form_source_type = 0;
        assert!(!p.form_valid());
    }

    #[test]
    fn form_valid_caldav_valid() {
        let mut p = test_page();
        p.form_name = "My Calendar".into();
        p.form_url = "https://dav.example.com/calendars/".into();
        p.form_source_type = 0;
        assert!(p.form_valid());
    }

    #[test]
    fn form_valid_ics_url_valid() {
        let mut p = test_page();
        p.form_name = "ICS Feed".into();
        p.form_url = "https://example.com/feed.ics".into();
        p.form_source_type = 1; // ICS URL
        assert!(p.form_valid());
    }

    #[test]
    fn form_valid_ics_file_nonexistent() {
        let mut p = test_page();
        p.form_name = "Local File".into();
        p.form_url = "/nonexistent/path/to/calendar.ics".into();
        p.form_source_type = 2; // ICS File
        assert!(!p.form_valid());
    }
}
