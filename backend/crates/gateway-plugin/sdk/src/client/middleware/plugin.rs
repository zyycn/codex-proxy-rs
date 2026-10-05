//! 将中间件处理函数适配为可注册的插件处理器

use std::future::Future;

use crate::{
    Capability, Contributions, ErrorCode, PluginFault, Stage,
    call::{middleware::HANDLE_METHOD, registration::Registration},
};

use super::{
    super::session::{CallFuture, CallReply, PluginCall, PluginHandler, SessionError},
    MiddlewareInput, MiddlewareOutput, RequestCall,
};

/// 单个中间件插件的作者入口；自动注册并将 RPC 分派到类型化业务函数
///
/// 传入作者清单的完整能力列表，不从宿主握手照抄未实现的能力
/// 此入口仅承载
/// 一个 middleware 处理器；复合插件仍使用 [`PluginHandler`]，不能虚报其他能力
/// 会话和生命周期分别由既有 SDK 会话与宿主负责，不在此建立第二套状态
///
/// # Examples
///
/// ```
/// use gateway_plugin_sdk::{Capability, ContributionDeclaration, Contributions, Stage};
/// use gateway_plugin_sdk::client::{RequestCall, MiddlewarePlugin};
///
/// let contributes = Contributions::from([(Capability::Middleware, ContributionDeclaration {
///     id: "acme.request-tags.tagRequest".into(),
///     version: 4,
///     stages: vec![Stage::Request],
///     input_formats: vec!["openai".into()],
///     output_formats: vec!["openai".into()],
/// })]);
/// let plugin = MiddlewarePlugin::new(&contributes, |call: RequestCall| async move {
///     let RequestCall { mut request, next, .. } = call;
///     request.append_header("x-team", b"research".to_vec());
///     next.run(request).await
/// })?;
/// // 将 plugin 交给 PluginSession::run；请求头修改直接作用于当前调用
/// # Ok::<(), gateway_plugin_sdk::client::SessionError>(())
/// ```
pub struct MiddlewarePlugin<F, C = RequestCall> {
    input: std::marker::PhantomData<fn(C)>,
    contributes: Contributions,
    handler: F,
}

impl<F, C, Fut> MiddlewarePlugin<F, C>
where
    C: MiddlewareInput,
    F: Fn(C) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<C::Output, PluginFault>> + Send + 'static,
{
    /// 绑定清单中唯一的中间件声明与业务处理函数
    ///
    /// # Errors
    ///
    /// 空声明、其他能力、重复能力、不支持的能力版本或挂载阶段返回配置错误
    /// 包格式与资源生命周期由宿主校验；入口不根据处理器代码推断清单
    pub fn new(contributes: &Contributions, handler: F) -> Result<Self, SessionError> {
        let Some(declaration) = contributes.get(&Capability::Middleware) else {
            return Err(SessionError::Configuration);
        };
        if contributes.len() != 1
            || !Capability::Middleware
                .contract_versions()
                .contains(&declaration.version)
            || declaration.stages.is_empty()
            || declaration.stages.len() > 5
            || declaration.stages.iter().any(|stage| {
                !matches!(
                    stage,
                    Stage::Http
                        | Stage::WebSocket
                        | Stage::Service
                        | Stage::Request
                        | Stage::Attempt
                ) || !C::accepts(*stage)
            })
            || declaration
                .stages
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != declaration.stages.len()
        {
            return Err(SessionError::Configuration);
        }
        Ok(Self {
            input: std::marker::PhantomData,
            contributes: contributes.clone(),
            handler,
        })
    }
}

impl<F, C, Fut> PluginHandler for MiddlewarePlugin<F, C>
where
    C: MiddlewareInput,
    F: Fn(C) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<C::Output, PluginFault>> + Send + 'static,
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
                    let call = C::decode(call)?;
                    (self.handler)(call).await?.encode()
                }
                _ => Err(PluginFault::new(
                    ErrorCode::Unsupported,
                    "plugin method is not supported",
                )),
            }
        })
    }
}
