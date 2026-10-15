//! 公开操作登记在所属服务；注册表只校验类型和分派，不实现业务

use crate::model::AdminError;
use futures::future::BoxFuture;
use gateway_core::engine::middleware::service::{Error, Value};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, sync::Arc};

type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, Error>> + Send + Sync>;

pub struct Registry {
    middleware: super::PlanSource,
    operations: BTreeMap<&'static str, Handler>,
}

impl Registry {
    pub fn new(middleware: super::PlanSource) -> Self {
        Self {
            middleware,
            operations: BTreeMap::new(),
        }
    }

    /// 网关与命令行复用同一组设置服务；注册不会执行读取或写入
    pub fn register_settings(
        &mut self,
        service: &Arc<dyn crate::SettingsService>,
    ) -> Result<(), AdminError> {
        super::settings::register(self, service)
    }

    pub(crate) fn register<I, O>(
        &mut self,
        name: &'static str,
        handler: impl Fn(I) -> BoxFuture<'static, Result<O, AdminError>> + Send + Sync + 'static,
    ) -> Result<(), AdminError>
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
    {
        let std::collections::btree_map::Entry::Vacant(entry) = self.operations.entry(name) else {
            return Err(AdminError::internal(format!("公开服务重复登记：{name}")));
        };
        let handler = Arc::new(handler);
        entry.insert(Arc::new(move |input| {
            let handler = handler.clone();
            Box::pin(async move {
                let input = serde_json::from_value(input)
                    .map_err(|_| Error::invalid("服务输入类型无效"))?;
                let output = handler(input).await.map_err(super::encode_error)?;
                serde_json::to_value(output).map_err(|_| Error::unavailable("服务结果编码失败"))
            })
        }));
        Ok(())
    }

    pub fn register_usage_data(
        &mut self,
        service: &Arc<dyn crate::UsageDataService>,
    ) -> Result<(), AdminError> {
        super::usage_data::register(self, service)
    }

    pub(crate) fn register_usage<I, O>(
        &mut self,
        name: &'static str,
        handler: impl Fn(I) -> BoxFuture<'static, crate::model::usage_data::UsageDataResult<O>>
        + Send
        + Sync
        + 'static,
    ) -> Result<(), AdminError>
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
    {
        let std::collections::btree_map::Entry::Vacant(entry) = self.operations.entry(name) else {
            return Err(AdminError::internal(format!("公开服务重复登记：{name}")));
        };
        let handler = Arc::new(handler);
        entry.insert(Arc::new(move |input| {
            let handler = handler.clone();
            Box::pin(async move {
                // 在反序列化之前限制编码后的完整请求，避免单条记录绕过批量行数约束
                let bytes =
                    serde_json::to_vec(&input).map_err(|_| Error::invalid("服务输入编码无效"))?;
                if bytes.len() > crate::model::usage_data::USAGE_DATA_MAX_REQUEST_BYTES {
                    return Err(super::usage_data::encode_error(
                        crate::model::usage_data::UsageDataError::new(
                            crate::model::usage_data::UsageErrorKind::CapacityExceeded,
                            "usage input exceeds 2 MiB",
                        ),
                    ));
                }
                let input = serde_json::from_value(input).map_err(|_| {
                    super::usage_data::encode_error(
                        crate::model::usage_data::UsageDataError::invalid("服务输入类型无效"),
                    )
                })?;
                let output = handler(input)
                    .await
                    .map_err(super::usage_data::encode_error)?;
                serde_json::to_value(output).map_err(|_| Error::unavailable("服务结果编码失败"))
            })
        }));
        Ok(())
    }

    pub async fn call(&self, operation: &str, input: Value) -> Result<Value, Error> {
        let (operation, handler) =
            self.operations
                .get_key_value(operation)
                .ok_or_else(|| Error {
                    details: None,
                    kind: "not_found".into(),
                    message: format!("公开服务不存在：{operation}"),
                })?;
        let handler = handler.clone();
        super::invoke(&self.middleware, operation, input, move |input| {
            handler(input)
        })
        .await
    }

    pub fn operations(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.operations.keys().copied()
    }
}
