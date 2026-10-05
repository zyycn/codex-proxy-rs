//! 校验插件请求头与响应头，并应用显式头部修改

use bytes::Bytes;
use gateway_core::engine::middleware::MiddlewareHeader;
use gateway_plugin_sdk::{
    PluginFault,
    call::middleware::{MiddlewareHeader as WireHeader, MiddlewareHeaderMutation},
};

use super::invalid;

const MAX_HEADERS: usize = 128;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_HEADER_TOTAL_BYTES: usize = 64 * 1024;

pub(super) fn apply_header_mutations(
    headers: &mut Vec<MiddlewareHeader>,
    mutations: &[MiddlewareHeaderMutation],
) -> Result<(), PluginFault> {
    // SDK 的完整替换会先删除原始集合，再追加新的集合
    if mutations.len() > MAX_HEADERS * 2 {
        return Err(invalid());
    }
    validate_headers(headers)?;
    for mutation in mutations {
        let (name, value) = match mutation {
            MiddlewareHeaderMutation::Remove { name } => (validated_header_name(name)?, None),
            MiddlewareHeaderMutation::Append { name, value } => {
                if value.len() > MAX_HEADER_VALUE_BYTES {
                    return Err(invalid());
                }
                (validated_header_name(name)?, Some(value.clone()))
            }
        };
        match value {
            None => headers.retain(|header| !header.name().eq_ignore_ascii_case(name)),
            Some(value) => headers.push(MiddlewareHeader::new(name.to_owned(), Bytes::from(value))),
        }
    }
    validate_headers(headers)
}

/// 保留完整 header 集合，只检查传输格式与资源大小
pub(super) fn validate_headers(headers: &[MiddlewareHeader]) -> Result<(), PluginFault> {
    if headers.len() > MAX_HEADERS {
        return Err(invalid());
    }
    let mut total = 0_usize;
    for header in headers {
        let name = validated_header_name(header.name())?;
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

pub(super) fn wire_headers(headers: &[MiddlewareHeader]) -> Result<Vec<WireHeader>, PluginFault> {
    validate_headers(headers)?;
    Ok(headers
        .iter()
        .map(|header| WireHeader {
            name: header.name().to_owned(),
            value: header.value().to_vec(),
        })
        .collect())
}

fn validated_header_name(name: &str) -> Result<&str, PluginFault> {
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
