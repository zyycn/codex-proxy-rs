use std::future::Future;

use crate::{
    Capability, Contributions, ErrorCode, PluginFault, Stage,
    call::{middleware::HANDLE_METHOD, registration::Registration},
};

use super::{
    super::session::{CallFuture, CallReply, PluginCall, PluginHandler, SessionError},
    MiddlewareCall, MiddlewareResponse,
};

/// 单个中间件插件的作者入口；自动注册并将 RPC 分派到类型化业务函数。
///
/// 传入作者清单的完整能力列表，不从宿主握手照抄未实现的能力。此入口仅承载
/// 一个 middleware 处理器；复合插件仍使用 [`PluginHandler`]，不能虚报其他能力。
/// 会话、授权和生命周期分别由既有 SDK 会话与宿主负责，不在此建立第二套状态。
///
/// # Examples
///
/// ```
/// use gateway_plugin_sdk::{Capability, ContributionDeclaration, Contributions, Stage};
/// use gateway_plugin_sdk::client::{MiddlewareCall, MiddlewarePlugin};
///
/// let contributes = Contributions::from([(Capability::Middleware, ContributionDeclaration {
///     id: "acme.request-tags.tagRequest".into(),
///     version: 1,
///     stages: vec![Stage::Request],
///     input_formats: vec!["openai".into()],
///     output_formats: vec!["openai".into()],
/// })]);
/// let plugin = MiddlewarePlugin::new(&contributes, |call: MiddlewareCall| async move {
///     let MiddlewareCall { mut request, next, .. } = call;
///     request.append_header("x-team", b"research".to_vec());
///     next.run(request).await
/// })?;
/// // 将 plugin 交给 PluginSession::run；请求头修改仍需管理员授权。
/// # Ok::<(), gateway_plugin_sdk::client::SessionError>(())
/// ```
pub struct MiddlewarePlugin<F> {
    contributes: Contributions,
    handler: F,
}

impl<F, Fut> MiddlewarePlugin<F>
where
    F: Fn(MiddlewareCall) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<MiddlewareResponse, PluginFault>> + Send + 'static,
{
    /// 绑定清单中唯一的中间件声明与业务处理函数。
    ///
    /// # Errors
    ///
    /// 空声明、其他能力、重复能力、不支持的能力版本或挂载阶段返回配置错误。
    /// 包格式、资源和权限仍由宿主校验；此处不根据代码推断或扩张授权。
    pub fn new(contributes: &Contributions, handler: F) -> Result<Self, SessionError> {
        let Some(declaration) = contributes.get(&Capability::Middleware) else {
            return Err(SessionError::Configuration);
        };
        if contributes.len() != 1
            || !matches!(declaration.version, 1 | 2)
            || declaration.stages.is_empty()
            || declaration.stages.len() > 2
            || declaration
                .stages
                .iter()
                .any(|stage| !matches!(stage, Stage::Request | Stage::Attempt))
            || (declaration.stages.len() == 2 && declaration.stages[0] == declaration.stages[1])
        {
            return Err(SessionError::Configuration);
        }
        Ok(Self {
            contributes: contributes.clone(),
            handler,
        })
    }
}

impl<F, Fut> PluginHandler for MiddlewarePlugin<F>
where
    F: Fn(MiddlewareCall) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<MiddlewareResponse, PluginFault>> + Send + 'static,
{
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let Some(declaration) = self.contributes.get(&Capability::Middleware) else {
                return Err(PluginFault::new(
                    ErrorCode::Fault,
                    "plugin middleware declaration is unavailable",
                ));
            };
            match call.method.as_str() {
                "plugin.register" => {
                    if call.context.stage != Stage::Registration
                        || !call
                            .params
                            .as_object()
                            .is_some_and(serde_json::Map::is_empty)
                        || !call.payload.is_empty()
                    {
                        return Err(super::invalid_input());
                    }
                    let registration = Registration {
                        contributes: self.contributes.clone(),
                    };
                    let result =
                        serde_json::to_value(registration).map_err(|_| super::invalid_input())?;
                    Ok(CallReply::unary(result, Vec::new()))
                }
                HANDLE_METHOD => {
                    if !declaration.stages.contains(&call.context.stage) {
                        return Err(super::invalid_input());
                    }
                    let call = MiddlewareCall::try_from(call)?;
                    (self.handler)(call).await?.into_reply()
                }
                _ => Err(PluginFault::new(
                    ErrorCode::Unsupported,
                    "plugin method is not supported",
                )),
            }
        })
    }
}
