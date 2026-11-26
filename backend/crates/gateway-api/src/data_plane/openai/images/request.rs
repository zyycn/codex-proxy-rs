//! 将图片编辑 multipart 上传映射到原生 Images JSON，保留图片字节与字段语义

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, FromRequest, Multipart, Request},
    http::{
        HeaderMap, HeaderValue,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use serde::ser::{SerializeMap, Serializer as _};
use serde_json::{Number, Value, json};

pub(super) async fn normalize_edit_body(
    body: Bytes,
    headers: &mut HeaderMap,
) -> Result<Bytes, GatewayError> {
    let is_multipart = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("multipart/form-data"));
    if !is_multipart {
        return Ok(body);
    }
    if headers.get_all(CONTENT_TYPE).iter().count() != 1 {
        return Err(invalid("Image edit requires a single Content-Type header."));
    }

    let mut request = Request::new(Body::from(body));
    *request.headers_mut() = headers.clone();
    // 原路由不设正文私有上限，重新提取时不能引入 Axum 默认的 2 MiB 限制
    DefaultBodyLimit::disable().apply(&mut request);
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|_| invalid("Image edit multipart boundary is invalid."))?;
    let mut body = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut body);
    // 直接序列化字段，避免经过 Map 后覆盖同名参数
    let mut fields = serializer.serialize_map(None).map_err(encode_error)?;
    let mut images = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("Image edit multipart body is invalid."))?
    {
        let name = field
            .name()
            .ok_or_else(|| invalid("Image edit multipart fields must have names."))?
            .to_owned();
        let is_file = field.file_name().is_some();
        let bytes = field
            .bytes()
            .await
            .map_err(|_| invalid("Image edit multipart field is invalid."))?;
        if matches!(name.as_str(), "image" | "image[]") {
            // 按表单中的出现顺序追加，多图顺序参与编辑语义
            images.push(image_reference(&bytes)?);
            continue;
        }
        let value = if name == "mask" {
            image_reference(&bytes)?
        } else {
            if is_file {
                return Err(invalid(
                    "Only image, image[] and mask may contain image files.",
                ));
            }
            scalar_field(&name, &bytes)?
        };
        fields
            .serialize_entry(&name, &value)
            .map_err(encode_error)?;
    }
    if images.is_empty() {
        return Err(invalid(
            "Image edit multipart requires an image or image[] upload.",
        ));
    }
    fields
        .serialize_entry("images", &images)
        .map_err(encode_error)?;
    fields.end().map_err(encode_error)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.remove(CONTENT_LENGTH);
    Ok(Bytes::from(body))
}

fn scalar_field(name: &str, bytes: &[u8]) -> Result<Value, GatewayError> {
    match name {
        "n" | "output_compression" | "partial_images" => serde_json::from_slice::<Number>(bytes)
            .map(Value::Number)
            .map_err(|_| invalid("Image edit numeric fields must contain JSON numbers.")),
        "stream" => serde_json::from_slice::<bool>(bytes)
            .map(Value::Bool)
            .map_err(|_| invalid("Image edit stream field must contain a JSON boolean.")),
        // 其余表单字段保留名称与文本值，不因字段尚未识别而丢弃或拒绝
        _ => {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| invalid("Image edit text fields must be valid UTF-8."))?;
            Ok(Value::String(text.to_owned()))
        }
    }
}

fn image_reference(bytes: &[u8]) -> Result<Value, GatewayError> {
    // 与官方 Original 路径一样保留 PNG/JPEG/WebP 原始字节；不依赖文件名或上传 MIME
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        return Err(invalid("Image edit uploads must be PNG, JPEG or WebP."));
    };
    Ok(json!({ "image_url": format!("data:{mime};base64,{}", STANDARD.encode(bytes)) }))
}

fn invalid(message: &'static str) -> GatewayError {
    GatewayError::new(GatewayErrorKind::InvalidRequest, message)
}

fn encode_error(_: serde_json::Error) -> GatewayError {
    GatewayError::new(
        GatewayErrorKind::Internal,
        "Could not encode image edit JSON.",
    )
}
