//! 数据面请求正文的有界解压与解码错误

use crate::client_error::{ProtocolError, ProtocolErrorBody};
use axum::http::HeaderMap;
use std::borrow::Cow;
use thiserror::Error;

/// 请求解码错误
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestDecodeError {
    /// 请求不是合法 JSON
    #[error("request body must be valid JSON")]
    MalformedJson,
    /// 请求体解压后超过允许上限
    #[error("decompressed request body is too large")]
    DecompressedBodyTooLarge {
        /// 当前请求快照中的解压输出上限
        limit_bytes: usize,
    },
    /// 请求使用了网关不支持的 `Content-Encoding`
    #[error("request content-encoding `{encoding}` is not supported")]
    UnsupportedContentEncoding {
        /// Content-Encoding 标识
        encoding: String,
    },
    /// 顶层不是 object
    #[error("request body must be a JSON object")]
    ExpectedObject,
    /// 缺少必填字段
    #[error("required field `{field}` is missing")]
    MissingField {
        /// 字段路径
        field: String,
    },
    /// 字段类型错误
    #[error("field `{field}` has an invalid type; expected {expected}")]
    InvalidType {
        /// 字段路径
        field: String,
        /// 安全的期望类型
        expected: &'static str,
    },
    /// 字段为空
    #[error("field `{field}` must not be empty")]
    EmptyField {
        /// 字段路径
        field: String,
    },
    /// 字段值不满足稳定约束
    #[error("field `{field}` has an invalid value")]
    InvalidValue {
        /// 字段路径；不保存原始值
        field: String,
    },
    /// 未知字段
    #[error("unknown field `{field}`")]
    UnknownField {
        /// 字段路径；不保存字段值
        field: String,
    },
    /// 已知但尚未实现的语义
    #[error("field `{field}` is not supported by this gateway")]
    UnsupportedField {
        /// 字段路径
        field: String,
    },
    /// 两个参数的要求无法同时满足
    #[error("requested `{field}` is not supported with `{other}`")]
    UnsupportedCombination {
        /// 返回给客户端的参数路径
        field: &'static str,
        /// 与之冲突的参数路径
        other: &'static str,
    },
    /// Core operation 拒绝规范化后的字段
    #[error("field `{field}` violates the canonical operation contract")]
    CanonicalContract {
        /// 字段路径
        field: String,
    },
}

impl RequestDecodeError {
    /// 转换为 OpenAI 风格且不泄露正文的错误 payload
    #[must_use]
    pub fn protocol_body(&self) -> ProtocolErrorBody {
        let (code, message, param) = match self {
            Self::MalformedJson => (
                "invalid_json",
                "Request body must be valid JSON.".to_owned(),
                None,
            ),
            Self::DecompressedBodyTooLarge { limit_bytes } => (
                "request_too_large",
                format!(
                    "Decompressed request body exceeds the allowed size ({limit_bytes} bytes)."
                ),
                None,
            ),
            Self::UnsupportedContentEncoding { encoding } => (
                "unsupported_content_encoding",
                format!("Content-Encoding `{encoding}` is not supported."),
                None,
            ),
            Self::ExpectedObject => (
                "invalid_request_body",
                "Request body must be a JSON object.".to_owned(),
                None,
            ),
            Self::MissingField { field } => (
                "missing_required_parameter",
                format!("Required parameter `{field}` is missing."),
                Some(field.clone()),
            ),
            Self::InvalidType { field, expected } => (
                "invalid_type",
                format!("Parameter `{field}` must be {expected}."),
                Some(field.clone()),
            ),
            Self::EmptyField { field } => (
                "invalid_value",
                format!("Parameter `{field}` must not be empty."),
                Some(field.clone()),
            ),
            Self::InvalidValue { field } | Self::CanonicalContract { field } => (
                "invalid_value",
                format!("Parameter `{field}` has an invalid value."),
                Some(field.clone()),
            ),
            Self::UnknownField { field } => (
                "unknown_parameter",
                format!("Unknown parameter `{field}`."),
                Some(field.clone()),
            ),
            Self::UnsupportedField { field } => (
                "unsupported_parameter",
                format!("Parameter `{field}` is not supported by this gateway."),
                Some(field.clone()),
            ),
            Self::UnsupportedCombination { field, other } => (
                "unsupported_parameter",
                format!("The requested `{field}` is not supported with `{other}`."),
                Some((*field).to_owned()),
            ),
        };
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "invalid_request_error",
                code,
                message,
                param,
            },
        }
    }
}

/// zstd 回溯窗口独立于输出上限，调整设置不能放大解码器内部窗口分配
const MAX_ZSTD_WINDOW_LOG: u32 = 26;

/// 按 `Content-Encoding` 解压下游请求体
///
/// 未压缩与 `identity` 借用原始正文；压缩正文在读取过程中限制展开大小，
/// 避免先完整分配再检查
/// 只接受单一编码，重复头和叠加编码不能只解释第一项
pub(in crate::data_plane) fn decompress_request_body<'a>(
    body: &'a [u8],
    headers: &HeaderMap,
    max_decompressed_bytes: usize,
) -> Result<Cow<'a, [u8]>, RequestDecodeError> {
    let encoding = headers
        .get_all(axum::http::header::CONTENT_ENCODING)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map_err(|_| RequestDecodeError::MalformedJson)
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Ok(Cow::Borrowed(body)),
        "gzip" => {
            let decoder = flate2::read::MultiGzDecoder::new(body);
            read_bounded(decoder, max_decompressed_bytes).map(Cow::Owned)
        }
        "deflate" => {
            let decoder = flate2::read::ZlibDecoder::new(body);
            read_bounded(decoder, max_decompressed_bytes).map(Cow::Owned)
        }
        "zstd" => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(body)
                .map_err(|_| RequestDecodeError::MalformedJson)?;
            // 输出有界之外，也限制压缩帧声明的回溯窗口，防止解码器内部过量分配
            decoder
                .window_log_max(MAX_ZSTD_WINDOW_LOG)
                .map_err(|_| RequestDecodeError::MalformedJson)?;
            read_bounded(decoder, max_decompressed_bytes).map(Cow::Owned)
        }
        other => Err(RequestDecodeError::UnsupportedContentEncoding {
            encoding: other.to_owned(),
        }),
    }
}

fn read_bounded<R: std::io::Read>(
    mut reader: R,
    max_decompressed_bytes: usize,
) -> Result<Vec<u8>, RequestDecodeError> {
    let mut decoded = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let remaining = max_decompressed_bytes - decoded.len();
        let read_limit = chunk.len().min(remaining.saturating_add(1));
        let read = reader
            .read(&mut chunk[..read_limit])
            .map_err(|_| RequestDecodeError::MalformedJson)?;
        if read == 0 {
            return Ok(decoded);
        }
        if read > remaining {
            return Err(RequestDecodeError::DecompressedBodyTooLarge {
                limit_bytes: max_decompressed_bytes,
            });
        }
        // 默认 Vec 扩容和 read_to_end 的 EOF 探测可能突破输出上限；
        // 容量增长也受相同边界约束，越界探测只使用栈上分块缓冲区
        let required = decoded.len() + read;
        if required > decoded.capacity() {
            let capacity = decoded
                .capacity()
                .saturating_mul(2)
                .max(required)
                .min(max_decompressed_bytes);
            decoded.reserve_exact(capacity - decoded.len());
        }
        decoded.extend_from_slice(&chunk[..read]);
    }
}
