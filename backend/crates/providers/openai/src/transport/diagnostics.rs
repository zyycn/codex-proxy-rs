//! Upstream response diagnostics captured at the transport boundary.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::header::HeaderMap;
use serde_json::Value;

const UPSTREAM_REQUEST_ID_HEADERS: &[&str] = &[
    "x-request-id",
    "x-oai-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
];

const UPSTREAM_TRACE_HEADERS: &[&str] = &[
    "x-request-id",
    "x-oai-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
    "cf-ray",
];
const IDENTITY_AUTHORIZATION_ERROR_HEADER: &str = "x-openai-authorization-error";
const IDENTITY_ERROR_JSON_HEADER: &str = "x-error-json";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexUpstreamDiagnostics {
    pub status_code: Option<u16>,
    pub request_id: Option<String>,
    pub identity_authorization_error: Option<String>,
    pub identity_error_code: Option<String>,
    pub trace_headers: Vec<(String, String)>,
}

impl CodexUpstreamDiagnostics {
    pub fn from_headers(status_code: Option<u16>, headers: &HeaderMap) -> Self {
        Self {
            status_code,
            request_id: first_header(headers, UPSTREAM_REQUEST_ID_HEADERS),
            identity_authorization_error: header_value(
                headers,
                IDENTITY_AUTHORIZATION_ERROR_HEADER,
            ),
            identity_error_code: header_value(headers, IDENTITY_ERROR_JSON_HEADER)
                .and_then(|encoded| decode_identity_error_code(&encoded)),
            trace_headers: trace_headers(headers),
        }
    }

    pub fn with_status(status_code: u16) -> Self {
        Self {
            status_code: Some(status_code),
            ..Self::default()
        }
    }

    pub fn from_pairs(status_code: Option<u16>, headers: &[(String, String)]) -> Self {
        let request_id = UPSTREAM_REQUEST_ID_HEADERS
            .iter()
            .find_map(|name| pair_value(headers, name));
        let trace_headers = UPSTREAM_TRACE_HEADERS
            .iter()
            .filter_map(|name| pair_value(headers, name).map(|value| ((*name).to_string(), value)))
            .collect();
        let identity_authorization_error = pair_value(headers, IDENTITY_AUTHORIZATION_ERROR_HEADER);
        let identity_error_code = pair_value(headers, IDENTITY_ERROR_JSON_HEADER)
            .and_then(|encoded| decode_identity_error_code(&encoded));
        Self {
            status_code,
            request_id,
            identity_authorization_error,
            identity_error_code,
            trace_headers,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.status_code.is_none()
            && self.request_id.is_none()
            && self.identity_authorization_error.is_none()
            && self.identity_error_code.is_none()
            && self.trace_headers.is_empty()
    }

    pub fn cf_ray(&self) -> Option<&str> {
        self.trace_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("cf-ray"))
            .map(|(_, value)| value.as_str())
    }
}

fn pair_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_header(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| header_value(headers, name))
}

fn trace_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    UPSTREAM_TRACE_HEADERS
        .iter()
        .filter_map(|name| header_value(headers, name).map(|value| ((*name).to_string(), value)))
        .collect()
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn decode_identity_error_code(encoded: &str) -> Option<String> {
    let decoded = STANDARD.decode(encoded).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .pointer("/error/code")?
        .as_str()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(ToString::to_string)
}
