//! Saved connection settings and connection-URL parsing.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};

/// How TLS is negotiated, matching libpq's `sslmode` of the same names.
///
/// None of the modes verify the server certificate, which is also libpq's
/// behaviour for `prefer` and `require`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    /// Plain TCP only.
    Disable,
    /// Try TLS, fall back to plain TCP if the server does not offer it.
    #[default]
    Prefer,
    /// TLS is required, the certificate is not verified.
    Require,
}

impl SslMode {
    pub const ALL: [SslMode; 3] = [SslMode::Disable, SslMode::Prefer, SslMode::Require];

    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "disable" | "allow" => Some(SslMode::Disable),
            "prefer" => Some(SslMode::Prefer),
            // verify-* are treated as `require`: Zeble does not verify certificates.
            "require" | "verify-ca" | "verify-full" => Some(SslMode::Require),
            _ => None,
        }
    }
}

/// Colour tag of a connection, shown in the list and in its window's top bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorTag {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl ColorTag {
    pub const ALL: [ColorTag; 7] = [
        ColorTag::Red,
        ColorTag::Orange,
        ColorTag::Yellow,
        ColorTag::Green,
        ColorTag::Blue,
        ColorTag::Purple,
        ColorTag::Gray,
    ];

    /// The colour as a 0xRRGGBB value.
    pub fn rgb(self) -> u32 {
        match self {
            ColorTag::Red => 0xe5484d,
            ColorTag::Orange => 0xf76b15,
            ColorTag::Yellow => 0xffc53d,
            ColorTag::Green => 0x30a46c,
            ColorTag::Blue => 0x0090ff,
            ColorTag::Purple => 0x8e4ec6,
            ColorTag::Gray => 0x8b8d98,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ColorTag::Red => "Red",
            ColorTag::Orange => "Orange",
            ColorTag::Yellow => "Yellow",
            ColorTag::Green => "Green",
            ColorTag::Blue => "Blue",
            ColorTag::Purple => "Purple",
            ColorTag::Gray => "Gray",
        }
    }
}

/// One saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Stored in plain text in `connections.json` (file mode 0600).
    #[serde(default)]
    pub password: String,
    /// When set, the password is this shell command's output instead of
    /// `password`; the output is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_command: Option<String>,
    pub database: String,
    #[serde(default)]
    pub ssl_mode: SslMode,
    #[serde(default)]
    pub color: Option<ColorTag>,
    /// When set, every session runs with `default_transaction_read_only = on`.
    #[serde(default)]
    pub read_only: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            password: String::new(),
            password_command: None,
            database: "postgres".into(),
            ssl_mode: SslMode::Prefer,
            color: None,
            read_only: false,
        }
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl ConnectionConfig {
    /// The name shown in lists and window titles, falling back to `user@host/db`.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.summary()
        } else {
            self.name.clone()
        }
    }

    /// `user@host:port/database`.
    pub fn summary(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }

    /// The password command, when the connection uses one.
    pub fn password_command(&self) -> Option<&str> {
        self.password_command
            .as_deref()
            .filter(|c| !c.trim().is_empty())
    }

    /// Check the fields needed to connect.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.host.trim().is_empty() {
            bail!("Host is required");
        }
        if self.user.trim().is_empty() {
            bail!("User is required");
        }
        if self.database.trim().is_empty() {
            bail!("Database is required");
        }
        Ok(())
    }

    /// Fill host, port, user, password, database and SSL mode from a
    /// `postgres://user:pass@host:port/db?sslmode=require` URL.
    ///
    /// Name, colour and read-only are kept. Missing port, user and database
    /// fall back to 5432, `postgres` and the user. A URL without a password
    /// leaves the password and password command alone; one with a password
    /// sets it and drops the command. Returns whether the URL had a password.
    pub fn apply_url(&mut self, input: &str) -> anyhow::Result<bool> {
        let input = input.trim();
        let url = url::Url::parse(input).context("Not a valid connection URL")?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            bail!("URL must start with postgres:// or postgresql://");
        }
        let decode = |s: &str| -> String {
            url::form_urlencoded::parse(format!("x={s}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        let host = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .to_string();
        self.host = if host.is_empty() {
            "localhost".into()
        } else {
            host
        };
        self.port = url.port().unwrap_or(5432);
        let user = decode(url.username());
        self.user = if user.is_empty() {
            "postgres".into()
        } else {
            user
        };
        let has_password = url.password().is_some();
        if let Some(p) = url.password() {
            self.password = decode(p);
            self.password_command = None;
        }
        let db = decode(url.path().trim_start_matches('/'));
        self.database = if db.is_empty() { self.user.clone() } else { db };
        for (k, v) in url.query_pairs() {
            if k == "sslmode" {
                self.ssl_mode =
                    SslMode::parse(&v).with_context(|| format!("Unknown sslmode \"{v}\""))?;
            }
        }
        Ok(has_password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_url() {
        let mut c = ConnectionConfig {
            name: "keep".into(),
            ..Default::default()
        };
        c.apply_url("postgresql://al%40ice:p%3Ass@db.example.com:6543/my%20db?sslmode=require")
            .unwrap();
        assert_eq!(c.name, "keep");
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 6543);
        assert_eq!(c.user, "al@ice");
        assert_eq!(c.password, "p:ss");
        assert_eq!(c.database, "my db");
        assert_eq!(c.ssl_mode, SslMode::Require);
    }

    #[test]
    fn url_password_replaces_command_and_missing_one_keeps_it() {
        let mut c = ConnectionConfig {
            password_command: Some("op read x".into()),
            ..Default::default()
        };
        assert!(!c.apply_url("postgres://bob@host/db").unwrap());
        assert_eq!(c.password_command(), Some("op read x"));
        assert!(c.apply_url("postgres://bob:pw@host/db").unwrap());
        assert_eq!(c.password, "pw");
        assert_eq!(c.password_command(), None);
    }

    #[test]
    fn url_defaults() {
        let mut c = ConnectionConfig::default();
        c.apply_url("postgres://bob@localhost").unwrap();
        assert_eq!(c.port, 5432);
        assert_eq!(c.database, "bob");
        assert_eq!(c.password, "");
    }

    #[test]
    fn rejects_other_schemes() {
        let mut c = ConnectionConfig::default();
        assert!(c.apply_url("mysql://x@y/z").is_err());
        assert!(c.apply_url("not a url").is_err());
        assert!(c.apply_url("postgres://x@y/z?sslmode=bogus").is_err());
    }
}
