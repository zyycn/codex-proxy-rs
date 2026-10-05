//! 在旧插件合同边界投影 Fast 设置并保留宿主三态语义

use gateway_core::{
    account::FastMode,
    settings::{ExecutionSettings, RequestSettings},
};
use gateway_plugin_sdk::{
    Capability, ErrorCode, PluginFault,
    call::{model::ExecutionEncodingError, upstream_adapter::UpstreamAdapterRequest},
};
use serde_json::Value;

use crate::RpcSession;

#[derive(Clone, Copy)]
pub(crate) struct FastSettings {
    legacy: bool,
}

impl FastSettings {
    pub(crate) fn middleware(session: &RpcSession) -> Self {
        Self {
            legacy: session.capability_version(Capability::Middleware) == Some(3),
        }
    }

    pub(crate) fn execution(self, settings: &RequestSettings) -> Result<Value, PluginFault> {
        let mut value = serde_json::to_value(settings.execution_values()).map_err(|_| invalid())?;
        if self.legacy {
            project_fast(&mut value)?;
        }
        Ok(value)
    }

    pub(crate) fn sources(self, settings: &RequestSettings) -> Result<Value, PluginFault> {
        let mut value = settings.inspect();
        if self.legacy
            && let Some(execution) = value["execution"].as_object_mut()
        {
            project_fast(execution.get_mut("input").ok_or_else(invalid)?)?;
            let mut source = execution.remove("fast_mode").ok_or_else(invalid)?;
            if !source.is_null() {
                source["value"] = disabled(&source["value"])?;
            }
            execution.insert("disable_fast".into(), source);
        }
        Ok(value)
    }

    pub(crate) fn decode(
        self,
        mut value: Value,
        current: &RequestSettings,
    ) -> Result<ExecutionSettings, PluginFault> {
        if self.legacy {
            let object = value.as_object_mut().ok_or_else(invalid)?;
            if object.contains_key("fast_mode") {
                return Err(invalid());
            }
            let next = object
                .remove("disable_fast")
                .and_then(|value| value.as_bool())
                .ok_or_else(invalid)?;
            let current = current.execution_values().ok_or_else(invalid)?.fast_mode;
            // 原样回传 false 不能把三态的 enabled 降为 default，只有实际变化才覆盖
            let mode = if next == (current == FastMode::Disabled) {
                current
            } else if next {
                FastMode::Disabled
            } else {
                FastMode::Default
            };
            object.insert("fast_mode".into(), Value::String(mode.as_str().into()));
        }
        serde_json::from_value(value).map_err(|_| invalid())
    }

    pub(crate) fn upstream_request(
        session: &RpcSession,
        request: UpstreamAdapterRequest,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, ExecutionEncodingError> {
        if session.capability_version(Capability::UpstreamAdapter) != Some(1) {
            return request.encode(body);
        }
        let mut metadata = serde_json::to_value(request).map_err(|_| ExecutionEncodingError)?;
        project_fast(&mut metadata).map_err(|_| ExecutionEncodingError)?;
        gateway_plugin_sdk::call::upstream_adapter::encode_request_metadata(&metadata, body)
    }
}

fn project_fast(value: &mut Value) -> Result<(), PluginFault> {
    if value.is_null() {
        return Ok(());
    }
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let mode = object.remove("fast_mode").ok_or_else(invalid)?;
    object.insert("disable_fast".into(), disabled(&mode)?);
    Ok(())
}

fn disabled(value: &Value) -> Result<Value, PluginFault> {
    let mode: FastMode = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    Ok(Value::Bool(mode == FastMode::Disabled))
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "invalid plugin execution settings")
}
