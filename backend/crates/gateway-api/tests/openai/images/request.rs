//! 图片上传与原生 JSON 的等价性、拒绝边界及请求中间件顺序测试

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

use super::{IMAGE_RESPONSE, ImageExecution, api_router};

const CONTENT_TYPE: &str = "multipart/form-data; boundary=image-boundary";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\xfforiginal png bytes\r\n";
const JPEG: &[u8] = b"\xff\xd8\xff\x00original jpeg bytes\xff\xd9";
const WEBP: &[u8] = b"RIFF\x10\x00\x00\x00WEBPoriginal webp bytes";

fn multipart_body(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, bytes) in parts {
        body.extend_from_slice(
            format!("--image-boundary\r\nContent-Disposition: form-data; name=\"{name}\"")
                .as_bytes(),
        );
        if let Some(filename) = filename {
            body.extend_from_slice(
                format!("; filename=\"{filename}\"\r\nContent-Type: application/octet-stream")
                    .as_bytes(),
            );
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--image-boundary--\r\n");
    body
}

#[tokio::test]
async fn multipart_edit_should_match_native_json_fields_and_image_bytes() {
    let execution = ImageExecution::new();
    let router = api_router(execution.clone()).await;
    let body = multipart_body(&[
        ("image", Some("first.bin"), PNG),
        ("prompt", None, "  保留空格\r\n\"和引号\"  ".as_bytes()),
        ("image[]", Some("second.bin"), JPEG),
        ("mask", Some("mask.bin"), PNG),
        ("image[]", Some("third.bin"), WEBP),
        ("model", None, b"gpt-image-2"),
        ("background", None, b"transparent"),
        ("quality", None, b"high"),
        ("size", None, b"1024x1024"),
        ("input_fidelity", None, b"high"),
        ("moderation", None, b"auto"),
        ("output_format", None, b"png"),
        ("response_format", None, b"b64_json"),
        ("user", None, b"000123"),
        ("safety_identifier", None, b"false"),
        ("n", None, b"2"),
        ("output_compression", None, b"80"),
        ("partial_images", None, b"0"),
        ("stream", None, b"false"),
    ]);
    let native_json = json!({
        "images": [
            {"image_url": format!("data:image/png;base64,{}", STANDARD.encode(PNG))},
            {"image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(JPEG))},
            {"image_url": format!("data:image/webp;base64,{}", STANDARD.encode(WEBP))},
        ],
        "mask": {"image_url": format!("data:image/png;base64,{}", STANDARD.encode(PNG))},
        "prompt": "  保留空格\r\n\"和引号\"  ",
        "model": "gpt-image-2",
        "background": "transparent",
        "quality": "high",
        "size": "1024x1024",
        "input_fidelity": "high",
        "moderation": "auto",
        "output_format": "png",
        "response_format": "b64_json",
        "user": "000123",
        "safety_identifier": "false",
        "n": 2,
        "output_compression": 80,
        "partial_images": 0,
        "stream": false,
    });
    for (content_type, body) in [
        (CONTENT_TYPE, body),
        (
            "application/json",
            serde_json::to_vec(&native_json).unwrap(),
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/v1/images/edits")
                    .header(AUTHORIZATION, "Bearer sk_images_test")
                    .header("content-type", content_type)
                    .header("content-length", body.len())
                    .header("x-codex-image-turn-id", "turn-multipart-edit")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            IMAGE_RESPONSE
        );
    }
    let captured = execution.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].context, captured[1].context);
    for request in &captured {
        let json: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(json, native_json);
        assert_eq!(request.context["image_turn_id"], "turn-multipart-edit");
        for (reference, original) in json["images"]
            .as_array()
            .unwrap()
            .iter()
            .zip([PNG, JPEG, WEBP])
        {
            let (_, encoded) = reference["image_url"]
                .as_str()
                .unwrap()
                .split_once(',')
                .unwrap();
            assert_eq!(STANDARD.decode(encoded).unwrap(), original);
        }
    }
}

#[tokio::test]
async fn multipart_edit_should_preserve_omitted_parameters_and_exact_numbers() {
    let execution = ImageExecution::new();
    let response = api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/images/edits")
                .header(AUTHORIZATION, "Bearer sk_images_test")
                .header("content-type", CONTENT_TYPE)
                .body(Body::from(multipart_body(&[
                    ("image", Some("image.png"), PNG),
                    ("prompt", None, b"false"),
                    // 参数值的可接受范围仍由上游决定，适配不能经浮点数中转损失精度
                    ("n", None, b"9007199254740993"),
                ])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let captured = execution.captured();
    let body: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "images": [{"image_url": format!("data:image/png;base64,{}", STANDARD.encode(PNG))}],
            "prompt": "false",
            "n": 9_007_199_254_740_993_u64,
        })
    );
}

#[tokio::test]
async fn multipart_edit_should_forward_unknown_fields_and_repeated_keys_in_order() {
    let execution = ImageExecution::new();
    let router = api_router(execution.clone()).await;
    let body = multipart_body(&[
        ("future_field", None, b"123"),
        ("prompt", None, b"first"),
        ("image", Some("image.png"), PNG),
        ("future_field", None, b"false"),
        ("prompt", None, b"second"),
        ("n", None, b"1"),
        ("n", None, b"2"),
        ("mask", Some("first.png"), PNG),
        ("mask", Some("second.webp"), WEBP),
    ]);
    let native_json = format!(
        concat!(
            r#"{{"future_field":"123","prompt":"first","future_field":"false","prompt":"second","n":1,"n":2,"#,
            r#""mask":{{"image_url":"data:image/png;base64,{}"}},"mask":{{"image_url":"data:image/webp;base64,{}"}},"#,
            r#""images":[{{"image_url":"data:image/png;base64,{}"}}]}}"#,
        ),
        STANDARD.encode(PNG),
        STANDARD.encode(WEBP),
        STANDARD.encode(PNG),
    );
    for (content_type, body) in [
        (CONTENT_TYPE, body),
        ("application/json", native_json.as_bytes().to_vec()),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/v1/images/edits")
                    .header(AUTHORIZATION, "Bearer sk_images_test")
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let captured = execution.captured();
    assert_eq!(captured.len(), 2);
    // Value 会折叠重复键，这里直接比较原生 JSON 字节，确认每次出现都保留
    assert_eq!(captured[0].body, captured[1].body);
    assert_eq!(captured[0].body.as_ref(), native_json.as_bytes());
}

#[tokio::test]
async fn multipart_edit_should_reject_fields_that_cannot_be_mapped_without_loss() {
    let cases = [
        (
            "invalid_number",
            vec![("n", None, b"not-a-number".as_slice())],
        ),
        ("invalid_boolean", vec![("stream", None, b"yes".as_slice())]),
        ("invalid_utf8", vec![("prompt", None, b"\xff".as_slice())]),
        (
            "file_in_scalar",
            vec![("prompt", Some("prompt.txt"), b"text".as_slice())],
        ),
        (
            "unsupported_image",
            vec![("image", Some("image.gif"), b"GIF89a".as_slice())],
        ),
        (
            "unsupported_mask",
            vec![("mask", Some("mask.png"), b"not an image".as_slice())],
        ),
    ];
    for (name, mut parts) in cases {
        parts.push(("image", Some("image.png"), PNG));
        let execution = ImageExecution::new();
        let response = api_router(execution.clone())
            .await
            .oneshot(
                Request::post("/v1/images/edits")
                    .header(AUTHORIZATION, "Bearer sk_images_test")
                    .header("content-type", CONTENT_TYPE)
                    .body(Body::from(multipart_body(&parts)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        let error: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(error["error"]["code"], "invalid_request", "{name}");
        assert!(execution.captured().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn multipart_edit_should_reject_invalid_envelopes_before_provider_execution() {
    for (content_type, body) in [
        (
            "multipart/form-data",
            multipart_body(&[("image", Some("image.png"), PNG)]),
        ),
        (CONTENT_TYPE, b"--image-boundary\r\ntruncated".to_vec()),
        (CONTENT_TYPE, multipart_body(&[("prompt", None, b"edit")])),
    ] {
        let execution = ImageExecution::new();
        let response = api_router(execution.clone())
            .await
            .oneshot(
                Request::post("/v1/images/edits")
                    .header(AUTHORIZATION, "Bearer sk_images_test")
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(execution.captured().is_empty());
    }
}

#[tokio::test]
async fn multipart_edit_should_not_inherit_the_default_extractor_size_limit() {
    let mut image = PNG.to_vec();
    image.resize(2 * 1024 * 1024 + 1, 0xff);
    let execution = ImageExecution::new();
    let response = api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/images/edits")
                .header(AUTHORIZATION, "Bearer sk_images_test")
                .header("content-type", CONTENT_TYPE)
                .body(Body::from(multipart_body(&[(
                    "image",
                    Some("image.png"),
                    &image,
                )])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let captured = execution.captured();
    let body: Value = serde_json::from_slice(&captured[0].body).unwrap();
    let (_, encoded) = body["images"][0]["image_url"]
        .as_str()
        .unwrap()
        .split_once(',')
        .unwrap();
    assert_eq!(STANDARD.decode(encoded).unwrap(), image);
}

#[tokio::test]
async fn request_middleware_should_run_before_multipart_decoding() {
    let mut execution = ImageExecution::new();
    let middleware = Arc::new(crate::openai::middleware::RequestMiddleware {
        expected_operation: Some(gateway_core::operation::OperationKind::GenerateImage),
        short_circuit_json: Some(bytes::Bytes::from_static(IMAGE_RESPONSE)),
        ..Default::default()
    });
    Arc::get_mut(&mut execution).unwrap().middleware = Some(middleware.clone());
    let response = api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/images/edits")
                .header(AUTHORIZATION, "Bearer sk_images_test")
                .header("content-type", CONTENT_TYPE)
                .body(Body::from("invalid multipart handled by middleware"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(*middleware.endpoints.lock().unwrap(), ["/v1/images/edits"]);
    assert!(execution.captured().is_empty());
}
