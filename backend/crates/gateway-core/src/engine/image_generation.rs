//! 冻结分组生图权限的请求与发送前门禁

use crate::{
    error::{GatewayError, GatewayErrorKind},
    operation::Operation,
};

pub(super) fn check_image_generation(
    allowed: bool,
    operation: &Operation,
) -> Result<(), GatewayError> {
    if !allowed && operation.image_generation_requested() {
        return Err(GatewayError::new(
            GatewayErrorKind::PolicyDenied,
            "Image generation is not allowed by the API key's account groups",
        )
        .with_client_code("image_generation_disabled"));
    }
    Ok(())
}
