//! Upstream response diagnostics captured at the transport boundary.

use reqwest::header::HeaderMap;

const UPSTREAM_REQUEST_ID_HEADERS: &[&str] = &[
    "x-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
];

const UPSTREAM_TRACE_HEADERS: &[&str] = &[
    "x-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
    "cf-ray",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexUpstreamDiagnostics {
    pub status_code: Option<u16>,
    pub request_id: Option<String>,
    pub trace_headers: Vec<(String, String)>,
}

impl CodexUpstreamDiagnostics {
    pub fn from_headers(status_code: Option<u16>, headers: &HeaderMap) -> Self {
        Self {
            status_code,
            request_id: first_header(headers, UPSTREAM_REQUEST_ID_HEADERS),
            trace_headers: trace_headers(headers),
        }
    }

    pub fn with_status(status_code: u16) -> Self {
        Self {
            status_code: Some(status_code),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.status_code.is_none() && self.request_id.is_none() && self.trace_headers.is_empty()
    }

    pub fn cf_ray(&self) -> Option<&str> {
        self.trace_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("cf-ray"))
            .map(|(_, value)| value.as_str())
    }
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

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::CodexUpstreamDiagnostics;

    #[test]
    fn diagnostics_should_extract_request_id_and_trace_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("req_1"));
        headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

        let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(429), &headers);

        assert_eq!(diagnostics.status_code, Some(429));
        assert_eq!(diagnostics.request_id.as_deref(), Some("req_1"));
        assert_eq!(diagnostics.cf_ray(), Some("ray_1"));
    }

    #[test]
    fn diagnostics_should_not_treat_cf_ray_as_request_id() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

        let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

        assert_eq!(diagnostics.request_id, None);
        assert_eq!(diagnostics.cf_ray(), Some("ray_1"));
    }
}
