//! Key 使用完整画像，或显式继承全局画像并附加 Provider 所有的字段

use std::borrow::Cow;

use crate::account::OpaqueProviderData;
use serde_json::{Map, Value};

/// `inherit` 是 Key 配置的外层协议；内部字段名和值只由对应 Provider 校验
#[must_use]
pub fn inherited_fields(key: &OpaqueProviderData) -> Option<&Map<String, Value>> {
    let document = key.expose_to_provider();
    (document.len() == 1)
        .then(|| document.get("inherit")?.as_object())
        .flatten()
}

/// 在请求快照上应用继承，不把全局字段复制回 Key；没有宿主配置时由 Provider 解析默认值
#[must_use]
pub fn resolve_key_profile<'a>(
    key: &'a OpaqueProviderData,
    global: Option<&OpaqueProviderData>,
) -> Cow<'a, OpaqueProviderData> {
    if let Some(fields) = inherited_fields(key)
        && let Some(global) = global
    {
        let mut resolved = global.clone().into_inner();
        resolved.extend(fields.clone());
        return Cow::Owned(OpaqueProviderData::new(resolved));
    }
    Cow::Borrowed(key)
}
