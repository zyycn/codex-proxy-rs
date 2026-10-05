//! 插件管理页面资源读取、请求转发与响应解码

mod callback;
mod registration;
mod validation;

use std::{collections::BTreeMap, sync::Arc};

use gateway_admin::model::{
    AdminError,
    plugins::management::{
        PluginManagementRequest, PluginManagementResponse, PluginManagementView,
    },
};
use gateway_plugin_sdk::{
    Stage,
    call::management::{ManagementRequest, ManagementResponse, ManagementRoute},
};

use crate::{RpcLimits, RpcReply, RpcSession, callback::PluginCallbacks};

pub(crate) use registration::prepare;

pub(crate) const MAXIMUM_BODY_BYTES: usize = 1024 * 1024;

pub(crate) struct ManagementEntry {
    pub(crate) view: PluginManagementView,
    routes: Vec<ManagementRoute>,
    resources: BTreeMap<String, Arc<[u8]>>,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
}

impl ManagementEntry {
    pub(crate) fn resource(
        &self,
        path: &str,
        public: bool,
    ) -> Result<PluginManagementResponse, AdminError> {
        let resource = self
            .view
            .resources
            .iter()
            .find(|resource| resource.path == path && (!public || resource.public))
            .ok_or_else(|| AdminError::not_found("插件资源不存在"))?;
        let body = self
            .resources
            .get(path)
            .ok_or_else(|| AdminError::not_found("插件资源不存在"))?;
        Ok(PluginManagementResponse {
            status: 200,
            content_type: resource.content_type.clone(),
            headers: Vec::new(),
            body: body.clone(),
        })
    }

    pub(crate) async fn handle(
        &self,
        request: PluginManagementRequest,
        limits: RpcLimits,
    ) -> Result<PluginManagementResponse, AdminError> {
        let route = self
            .routes
            .iter()
            .find(|route| route.method == request.method && route.path == request.path)
            .ok_or_else(|| AdminError::not_found("插件管理路由不存在"))?;
        let maximum_body = MAXIMUM_BODY_BYTES.min(limits.maximum_buffered_body_bytes);
        if request.body.len() > maximum_body
            || request.query.len() > 8192
            || request.query.bytes().any(|byte| byte.is_ascii_control())
            || request
                .content_type
                .as_ref()
                .is_some_and(|value| !route.request_content_types.contains(value))
            || (!request.body.is_empty() && request.content_type.is_none())
            || (matches!(route.method.as_str(), "GET" | "HEAD") && !request.body.is_empty())
        {
            return Err(AdminError::invalid("插件管理请求正文或内容类型无效"));
        }
        let params = serde_json::to_value(ManagementRequest {
            method: request.method,
            path: request.path,
            query: request.query,
            content_type: request.content_type,
            headers: request
                .headers
                .iter()
                .map(
                    |header| gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                        name: header.name().to_owned(),
                        value: header.value().to_vec(),
                    },
                )
                .collect(),
        })
        .map_err(|_| AdminError::invalid("插件管理请求无法编码"))?;
        let mut context = self
            .session
            .context(Stage::Management, limits.maximum_call_timeout);
        context.request_id = Some(request.request_id);
        let _scope = self
            .callbacks
            .prepare_management(&context, None)
            .map_err(|_| AdminError::invalid("插件管理回调范围无效"))?;
        let reply = self
            .session
            .call("management.handle", context, params, request.body)
            .await
            .map_err(|_| {
                AdminError::unavailable("插件管理调用未完成；副作用可能已发生，请查询后再重试")
            })?;
        decode_response(reply, &route.response_content_types, limits)
    }
}

// 管理 API 与公开回调共用类型和正文预算校验，完整响应头由 HTTP 入口解码
fn decode_response(
    reply: RpcReply,
    allowed_content_types: &[String],
    limits: RpcLimits,
) -> Result<PluginManagementResponse, AdminError> {
    let response: ManagementResponse = serde_json::from_value(reply.result)
        .map_err(|_| AdminError::unavailable("插件管理响应元数据无效"))?;
    if !(200..=599).contains(&response.status)
        || !allowed_content_types.contains(&response.content_type)
        || reply.payload.len() > MAXIMUM_BODY_BYTES.min(limits.maximum_buffered_body_bytes)
        || (matches!(response.status, 204 | 304) && !reply.payload.is_empty())
    {
        return Err(AdminError::unavailable(
            "插件管理响应状态、内容类型或正文无效",
        ));
    }
    Ok(PluginManagementResponse {
        status: response.status,
        content_type: response.content_type,
        headers: response
            .headers
            .into_iter()
            .map(|header| {
                gateway_core::engine::middleware::MiddlewareHeader::new(
                    header.name,
                    header.value.into(),
                )
            })
            .collect(),
        body: reply.payload.into(),
    })
}
