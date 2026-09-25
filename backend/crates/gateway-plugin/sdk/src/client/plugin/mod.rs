//! 可组合的类型化作者入口，注册事实始终来自同一份规范化清单。

pub mod methods;
mod typed;
mod validation;

use std::{collections::BTreeMap, future::Future, sync::Arc};

use crate::{
    Capability, ErrorCode, Manifest, PluginFault, Stage,
    call::{
        management::{
            CommandInvocation, CommandRegistration, CommandResult, ManagementRegistration,
            ManagementRequest, ManagementResponse,
        },
        registration::Registration,
    },
};

use super::{CallFuture, CallReply, MiddlewareCall, MiddlewareResponse, PluginCall, PluginHandler};

pub use typed::{Empty, Method, TypedCall, TypedReply};

type Handler = Arc<dyn Fn(PluginCall) -> CallFuture<'static> + Send + Sync>;

/// 作者配置错误只包含方法与能力标识，不输出清单中的敏感业务数据。
#[derive(Debug, thiserror::Error)]
pub enum AuthorError {
    #[error("plugin author manifest is invalid: {0}")]
    Manifest(#[from] crate::ManifestError),
    #[error("plugin method is duplicated or not declared: {0}")]
    Method(&'static str),
    #[error("plugin method is required: {0}")]
    MissingMethod(&'static str),
}

/// 从作者清单组合处理器；不手写 `plugin.register` 或复制宿主握手的能力。
pub struct PluginBuilder {
    manifest: Manifest,
    handlers: BTreeMap<&'static str, Handler>,
}

impl PluginBuilder {
    /// 读取与 `cpr-plugin package --manifest` 相同的作者清单。
    ///
    /// # Errors
    ///
    /// 作者清单无效、含构建元数据或未显式选择中间件阶段时失败。
    pub fn from_json(source: &[u8]) -> Result<Self, AuthorError> {
        Self::from_manifest(Manifest::from_author_slice(source)?)
    }

    /// 使用已经规范化的清单。通常优先使用 `from_json(include_bytes!(...))`。
    ///
    /// # Errors
    ///
    /// 清单校验失败时返回错误。
    pub fn from_manifest(manifest: Manifest) -> Result<Self, AuthorError> {
        manifest.validate()?;
        Ok(Self {
            manifest,
            handlers: BTreeMap::new(),
        })
    }

    /// 注册 SDK 定义的类型化方法，参数／载荷编码由方法合同决定。
    ///
    /// # Errors
    ///
    /// 重复注册或对应能力未在作者清单声明时失败。
    pub fn on<P, R, F, Fut>(mut self, method: Method<P, R>, handler: F) -> Result<Self, AuthorError>
    where
        P: Send + 'static,
        R: Send + 'static,
        F: Fn(TypedCall<P>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TypedReply<R>, PluginFault>> + Send + 'static,
    {
        let declared = if method.capabilities.is_empty() {
            method.name == methods::STATE_MIGRATE.name
                && self
                    .manifest
                    .state
                    .iter()
                    .any(|namespace| !namespace.migrates_from.is_empty())
        } else {
            method
                .capabilities
                .iter()
                .any(|capability| self.manifest.contributes.contains_key(capability))
        };
        if !declared {
            return Err(AuthorError::Method(method.name));
        }
        let handler = Arc::new(handler);
        self.insert(
            method.name,
            Arc::new(move |call| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    let call = (method.decode)(call, method.stages)?;
                    (method.encode)(handler(call).await?)
                })
            }),
        )?;
        Ok(self)
    }

    /// 组合洋葱中间件，沿用 single-use next 与惰性正文，不建立另一套流式机制。
    ///
    /// # Errors
    ///
    /// 未声明中间件或重复注册时失败。
    pub fn middleware<F, Fut>(mut self, handler: F) -> Result<Self, AuthorError>
    where
        F: Fn(MiddlewareCall) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<MiddlewareResponse, PluginFault>> + Send + 'static,
    {
        let declaration = self
            .manifest
            .contributes
            .get(&Capability::Middleware)
            .ok_or(AuthorError::Method(crate::call::middleware::HANDLE_METHOD))?;
        let stages = declaration.stages.clone();
        let handler = Arc::new(handler);
        self.insert(
            crate::call::middleware::HANDLE_METHOD,
            Arc::new(move |call| {
                let handler = Arc::clone(&handler);
                let allowed = stages.contains(&call.context.stage);
                Box::pin(async move {
                    if !allowed {
                        return Err(typed::invalid_input());
                    }
                    handler(MiddlewareCall::try_from(call)?).await?.into_reply()
                })
            }),
        )?;
        Ok(self)
    }

    /// 冻结模型别名目录；目标校验与发布由宿主负责。
    ///
    /// # Errors
    ///
    /// 未声明模型目录能力或重复注册时失败。
    pub fn model_catalog(
        self,
        registration: crate::call::catalog::ModelCatalogRegistration,
    ) -> Result<Self, AuthorError> {
        self.on(methods::MODEL_CATALOG_REGISTER, move |_| {
            let registration = registration.clone();
            async move { Ok(TypedReply::new(registration)) }
        })
    }

    /// 冻结管理页面／路由描述并绑定类型化业务函数。
    ///
    /// # Errors
    ///
    /// 未声明管理能力或重复注册时失败。
    pub fn management<F, Fut>(
        self,
        registration: ManagementRegistration,
        handler: F,
    ) -> Result<Self, AuthorError>
    where
        F: Fn(TypedCall<ManagementRequest>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TypedReply<ManagementResponse>, PluginFault>> + Send + 'static,
    {
        self.on(methods::MANAGEMENT_REGISTER, move |_| {
            let registration = registration.clone();
            async move { Ok(TypedReply::new(registration)) }
        })?
        .on(methods::MANAGEMENT_HANDLE, handler)
    }

    /// 冻结命令帮助并绑定类型化命令执行函数。
    ///
    /// # Errors
    ///
    /// 未声明命令行能力或重复注册时失败。
    pub fn command_line<F, Fut>(
        self,
        registration: CommandRegistration,
        handler: F,
    ) -> Result<Self, AuthorError>
    where
        F: Fn(TypedCall<CommandInvocation>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TypedReply<CommandResult>, PluginFault>> + Send + 'static,
    {
        self.on(methods::COMMAND_LINE_REGISTER, move |_| {
            let registration = registration.clone();
            async move { Ok(TypedReply::new(registration)) }
        })?
        .on(methods::COMMAND_LINE_EXECUTE, handler)
    }

    /// 检查声明与实际处理器后生成可交给 `PluginSession::run` 的插件。
    ///
    /// # Errors
    ///
    /// 声明缺少处理器时失败。
    pub fn build(self) -> Result<ComposedPlugin, AuthorError> {
        validation::validate(&self.manifest, &self.handlers)?;
        Ok(ComposedPlugin {
            registration: Registration {
                contributes: self.manifest.contributes,
            },
            handlers: self.handlers,
        })
    }

    fn insert(&mut self, method: &'static str, handler: Handler) -> Result<(), AuthorError> {
        if self.handlers.contains_key(method) {
            return Err(AuthorError::Method(method));
        }
        self.handlers.insert(method, handler);
        Ok(())
    }
}

/// 已校验的业务处理器集合；会话仍负责取消、回调和流控。
pub struct ComposedPlugin {
    registration: Registration,
    handlers: BTreeMap<&'static str, Handler>,
}

impl PluginHandler for ComposedPlugin {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            if call.method == "plugin.register" {
                if call.context.stage != Stage::Registration
                    || !call.payload.is_empty()
                    || !call
                        .params
                        .as_object()
                        .is_some_and(serde_json::Map::is_empty)
                {
                    return Err(typed::invalid_input());
                }
                return Ok(CallReply::unary(
                    serde_json::to_value(&self.registration).map_err(|_| typed::invalid_input())?,
                    Vec::new(),
                ));
            }
            let handler = self.handlers.get(call.method.as_str()).ok_or_else(|| {
                PluginFault::new(ErrorCode::Unsupported, "plugin method is not supported")
            })?;
            handler(call).await
        })
    }
}
