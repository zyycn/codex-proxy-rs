//! Codex standalone Search 请求的定向字段兼容。

use std::fmt;

use bytes::Bytes;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;

const RESPONSES_CACHE_KEY: &str = "prompt_cache_key";

/// 中间层可能把含 input 的 Search 误判为 Responses 并补入缓存键；官方
/// SearchRequest 没有该字段，上游会拒绝。这里只处理顶层，不推断其他字段。
pub(crate) fn prepare_search_body(body: &Bytes) -> Bytes {
    let Ok(mut object) = serde_json::from_slice::<SearchBody<'_>>(body) else {
        // 非对象或损坏的正文继续由上游校验，避免兼容处理改变原有错误合同。
        return body.clone();
    };
    let original_len = object.0.len();
    object.0.retain(|(key, _)| key != RESPONSES_CACHE_KEY);
    if object.0.len() == original_len {
        return body.clone();
    }
    serde_json::to_vec(&object)
        .map(Bytes::from)
        .unwrap_or_else(|_| body.clone())
}

// 有序条目保留顶层重复键，RawValue 保留嵌套正文、数值和字符串转义；
// 不能用 JSON Map 重建，否则会合并重复键并改写其他字段值。
struct SearchBody<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for SearchBody<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SearchBodyVisitor)
    }
}

struct SearchBodyVisitor;

impl<'de> Visitor<'de> for SearchBodyVisitor {
    type Value = SearchBody<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a search request JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry::<String, &'de RawValue>()? {
            entries.push(entry);
        }
        Ok(SearchBody(entries))
    }
}

impl Serialize for SearchBody<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
    }
}
