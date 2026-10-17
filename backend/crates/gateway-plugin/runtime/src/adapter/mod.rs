//! 将插件贡献适配为宿主认证、目录、策略、管理与执行能力

pub(super) mod catalog;
pub(super) mod command_line;
pub(super) mod frontend_authentication;
pub(super) mod management;
pub(super) mod middleware;
pub(super) mod observer;
pub(super) mod policy;
pub(super) mod scope;
pub(super) mod upstream_adapter;

/// 公共 RPC 上下文统一投影入口快照，不按能力版本裁剪字段
pub(crate) fn request_context(
    request: Option<&gateway_core::engine::extensions::RequestContext>,
) -> Option<std::sync::Arc<gateway_plugin_sdk::RequestContext>> {
    request.map(|request| {
        std::sync::Arc::new(gateway_plugin_sdk::RequestContext {
            method: request.method.clone(),
            uri: request.uri.clone(),
            http_version: request.http_version.clone(),
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
            peer_addr: request.peer_addr.map(|address| address.to_string()),
            client_ip: request.client_ip.map(|address| address.to_string()),
        })
    })
}
