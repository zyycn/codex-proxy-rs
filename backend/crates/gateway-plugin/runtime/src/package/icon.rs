//! 校验插件图标路径、格式、尺寸与动画帧资源限制

use std::{collections::BTreeMap, io::Cursor};

use gateway_admin::model::plugins::PluginIconTheme;
use gateway_plugin_sdk::{Manifest, PluginIcon};
use image::{
    AnimationDecoder as _, Frames, ImageDecoder as _, ImageFormat, ImageReader, Limits,
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
};

use super::PackageError;

const MAXIMUM_ICON_BYTES: usize = 512 * 1024;
const MAXIMUM_ICON_DIMENSION: u32 = 4096;
const MAXIMUM_ICON_ALLOCATION: u64 = 128 * 1024 * 1024;
const MAXIMUM_ICON_FRAMES: usize = 256;
const MAXIMUM_SVG_NODES: u32 = 16_384;

pub(super) fn validate(
    manifest: &Manifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    let Some(icon) = &manifest.icon else {
        return Ok(());
    };
    match icon {
        PluginIcon::Path(path) => validate_path(manifest, files, path),
        PluginIcon::Themed(variants) => {
            validate_path(manifest, files, &variants.light)?;
            validate_path(manifest, files, &variants.dark)
        }
    }
}

pub(super) fn path(manifest: &Manifest, theme: PluginIconTheme) -> Option<&str> {
    match manifest.icon.as_ref()? {
        PluginIcon::Path(path) => Some(path),
        PluginIcon::Themed(variants) => Some(match theme {
            PluginIconTheme::Light => &variants.light,
            PluginIconTheme::Dark => &variants.dark,
        }),
    }
}

fn validate_path(
    manifest: &Manifest,
    files: &BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<(), PackageError> {
    let bytes = files.get(path).ok_or(PackageError::Archive)?;
    if bytes.len() > MAXIMUM_ICON_BYTES {
        return Err(PackageError::Limit);
    }
    if manifest.resources.get(path).map(String::as_str) == Some("image/svg+xml") {
        return validate_svg(bytes);
    }
    // 扩展名与 MIME 的对应关系由 SDK 清单校验拥有，这里只核对实际编码与解码预算
    let expected = ImageFormat::from_path(path).map_err(|_| PackageError::Archive)?;
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PackageError::Archive)?;
    if reader.format() != Some(expected) {
        return Err(PackageError::Archive);
    }
    let mut header = ImageReader::with_format(Cursor::new(bytes), expected);
    header.limits(icon_limits());
    let (width, height) = header.into_dimensions().map_err(map_image_error)?;
    validate_dimensions(width, height)?;
    // 动画逐帧检查但不收集帧，避免为了验证小图标保留整段动画的解码内存
    match expected {
        ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(Cursor::new(bytes), icon_limits())
                .map_err(map_image_error)?;
            if decoder.is_apng().map_err(map_image_error)? {
                return validate_frames(decoder.apng().map_err(map_image_error)?.into_frames());
            }
        }
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(map_image_error)?;
            if decoder.has_animation() {
                return validate_frames(decoder.into_frames());
            }
        }
        ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(map_image_error)?;
            decoder.set_limits(icon_limits()).map_err(map_image_error)?;
            return validate_frames(decoder.into_frames());
        }
        _ => {}
    }
    reader.limits(icon_limits());
    let image = reader.decode().map_err(map_image_error)?;
    if image.width() != width || image.height() != height {
        return Err(PackageError::Archive);
    }
    Ok(())
}

fn validate_svg(bytes: &[u8]) -> Result<(), PackageError> {
    let text = std::str::from_utf8(bytes).map_err(|_| PackageError::Archive)?;
    // 不解析 DTD 或外部实体；保留正常 SVG 样式与矢量内容，由图像上下文和接口 CSP 隔离主动内容
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: MAXIMUM_SVG_NODES,
            ..Default::default()
        },
    )
    .map_err(|error| match error {
        roxmltree::Error::NodesLimitReached => PackageError::Limit,
        _ => PackageError::Archive,
    })?;
    if !document
        .root_element()
        .has_tag_name(("http://www.w3.org/2000/svg", "svg"))
        || document.descendants().any(|node| node.is_pi())
    {
        return Err(PackageError::Archive);
    }
    Ok(())
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), PackageError> {
    if width == 0
        || height == 0
        || width > MAXIMUM_ICON_DIMENSION
        || height > MAXIMUM_ICON_DIMENSION
    {
        return Err(PackageError::Limit);
    }
    Ok(())
}

fn validate_frames(frames: Frames<'_>) -> Result<(), PackageError> {
    let mut decoded_bytes = 0_u64;
    let mut frame_count = 0;
    for (index, frame) in frames.enumerate() {
        if index >= MAXIMUM_ICON_FRAMES {
            return Err(PackageError::Limit);
        }
        let frame = frame.map_err(map_image_error)?;
        let buffer = frame.buffer();
        validate_dimensions(buffer.width(), buffer.height())?;
        decoded_bytes += buffer.as_raw().len() as u64;
        if decoded_bytes > MAXIMUM_ICON_ALLOCATION {
            return Err(PackageError::Limit);
        }
        frame_count += 1;
    }
    (frame_count > 0).then_some(()).ok_or(PackageError::Archive)
}

fn icon_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAXIMUM_ICON_ALLOCATION);
    limits
}

fn map_image_error(error: image::ImageError) -> PackageError {
    if matches!(error, image::ImageError::Limits(_)) {
        PackageError::Limit
    } else {
        PackageError::Archive
    }
}
