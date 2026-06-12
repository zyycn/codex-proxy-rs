use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliAuthImportError {
    #[error("CODEX_HOME is not set and HOME is unavailable")]
    MissingHome,
    #[error("failed to read CLI auth file {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to parse CLI auth.json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("CLI auth.json does not contain access_token")]
    MissingAccessToken,
}

#[derive(Debug, Clone)]
pub struct CliAuth {
    access_token: SecretString,
    refresh_token: Option<SecretString>,
    id_token: Option<SecretString>,
    expires_at: Option<i64>,
}

impl CliAuth {
    pub fn access_token(&self) -> &str {
        self.access_token.expose_secret()
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token
            .as_ref()
            .map(|token| token.expose_secret())
    }

    pub fn id_token(&self) -> Option<&str> {
        self.id_token.as_ref().map(|token| token.expose_secret())
    }

    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }
}

#[derive(Debug, Deserialize)]
struct CliAuthJson {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
}

pub fn default_codex_home() -> Result<PathBuf, CliAuthImportError> {
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(codex_home));
    }
    let home = env::var_os("HOME").ok_or(CliAuthImportError::MissingHome)?;
    Ok(PathBuf::from(home).join(".codex"))
}

pub fn read_cli_auth_from_home(codex_home: &Path) -> Result<CliAuth, CliAuthImportError> {
    let auth_path = codex_home.join("auth.json");
    let raw = fs::read_to_string(&auth_path).map_err(|source| CliAuthImportError::Read {
        path: auth_path,
        source,
    })?;
    parse_cli_auth_json(&raw)
}

pub fn parse_cli_auth_json(raw: &str) -> Result<CliAuth, CliAuthImportError> {
    let parsed = serde_json::from_str::<CliAuthJson>(raw)?;
    let access_token =
        non_empty_secret(parsed.access_token).ok_or(CliAuthImportError::MissingAccessToken)?;

    Ok(CliAuth {
        access_token,
        refresh_token: non_empty_secret(parsed.refresh_token),
        id_token: non_empty_secret(parsed.id_token),
        expires_at: parsed.expires_at,
    })
}

fn non_empty_secret(value: Option<String>) -> Option<SecretString> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| SecretString::new(value.into()))
}
