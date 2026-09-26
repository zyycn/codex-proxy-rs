use bytes::Bytes;
use gateway_core::engine::middleware::MiddlewareHeader;
use gateway_plugin_sdk::{
    PluginFault,
    call::middleware::{MiddlewareHeader as WireHeader, MiddlewareHeaderMutation},
};
use gateway_protocol::openai::{
    is_transport_managed_request_header, response_header_is_forwardable,
};

use super::{denied, invalid};

const MAX_HEADERS: usize = 128;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_HEADER_TOTAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub(super) enum HeaderDirection {
    Request,
    Response,
}

pub(super) fn apply_header_mutations(
    headers: &mut Vec<MiddlewareHeader>,
    mutations: &[MiddlewareHeaderMutation],
    direction: HeaderDirection,
) -> Result<(), PluginFault> {
    if mutations.len() > MAX_HEADERS {
        return Err(invalid());
    }
    validate_headers(headers)?;
    let connection_options = connection_options(headers);
    for mutation in mutations {
        let (name, value) = match mutation {
            MiddlewareHeaderMutation::Remove { name } => (normalized_header_name(name)?, None),
            MiddlewareHeaderMutation::Append { name, value } => {
                if value.len() > MAX_HEADER_VALUE_BYTES {
                    return Err(invalid());
                }
                (normalized_header_name(name)?, Some(value.clone()))
            }
        };
        if name == "x-gateway-request-id"
            || !header_is_visible(&name, direction)
            || connection_options.iter().any(|option| option == &name)
        {
            return Err(denied());
        }
        match value {
            None => headers.retain(|header| !header.name().eq_ignore_ascii_case(&name)),
            Some(value) => headers.push(MiddlewareHeader::new(name, Bytes::from(value))),
        }
    }
    validate_headers(headers)
}

/// 原始头保留给宿主；这里检查格式与大小，可见性和写权限由各自边界控制。
pub(super) fn validate_headers(headers: &[MiddlewareHeader]) -> Result<(), PluginFault> {
    if headers.len() > MAX_HEADERS {
        return Err(invalid());
    }
    let mut total = 0_usize;
    for header in headers {
        let name = normalized_header_name(header.name())?;
        if header.value().len() > MAX_HEADER_VALUE_BYTES
            || header
                .value()
                .iter()
                .any(|byte| *byte != b'\t' && (*byte < b' ' || *byte == 0x7f))
        {
            return Err(invalid());
        }
        total = total
            .checked_add(name.len())
            .and_then(|value| value.checked_add(header.value().len()))
            .ok_or_else(invalid)?;
        if total > MAX_HEADER_TOTAL_BYTES {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn project_headers(
    headers: &[MiddlewareHeader],
    direction: HeaderDirection,
) -> Result<Vec<WireHeader>, PluginFault> {
    validate_headers(headers)?;
    let connection_options = connection_options(headers);
    let mut projected = Vec::with_capacity(headers.len());
    for header in headers {
        let name = normalized_header_name(header.name())?;
        if header_is_visible(&name, direction)
            && !connection_options.iter().any(|option| option == &name)
        {
            projected.push(WireHeader {
                name,
                value: header.value().to_vec(),
            });
        }
    }
    Ok(projected)
}

fn normalized_header_name(name: &str) -> Result<String, PluginFault> {
    let name = name.to_ascii_lowercase();
    if name.is_empty()
        || name.len() > MAX_HEADER_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return Err(invalid());
    }
    Ok(name)
}

fn header_is_visible(name: &str, direction: HeaderDirection) -> bool {
    !sensitive_or_identity_header(name)
        && match direction {
            HeaderDirection::Request => !is_transport_managed_request_header(name),
            HeaderDirection::Response => response_header_is_forwardable(name, &[]),
        }
}

fn connection_options(headers: &[MiddlewareHeader]) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("connection"))
        .filter_map(|header| std::str::from_utf8(header.value()).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn sensitive_or_identity_header(name: &str) -> bool {
    name == "x-openai-fedramp"
        || name.contains("auth")
        || name.contains("credential")
        || name.contains("secret")
        || name.contains("token")
        || name.contains("cookie")
        || name.contains("session")
        || name.contains("conversation")
        || name.contains("thread")
        || name.contains("account")
        || name.contains("organization")
        || name.contains("project")
        || name.contains("tenant")
        || name.contains("principal")
        || name.contains("identity")
        || name.contains("user-id")
        || name.ends_with("-key")
        || name.ends_with("_key")
}
