//! 只借用路径命中的 JSON 字段，跳过无关值，不展开工具或历史

use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{borrow::Cow, fmt};

#[derive(Deserialize)]
struct Text<'a>(#[serde(borrow)] Cow<'a, str>);

pub(super) fn first(raw: &RawValue) -> Option<&RawValue> {
    let array = raw.get().strip_prefix('[')?;
    serde_json::Deserializer::from_str(array)
        .into_iter::<&RawValue>()
        .next()?
        .ok()
}

pub(super) fn field<'a>(raw: &'a RawValue, name: &str) -> Option<&'a RawValue> {
    struct Field<'a>(&'a str);
    impl<'de> Visitor<'de> for Field<'_> {
        type Value = Option<&'de RawValue>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut found = None;
            while let Some(key) = map.next_key::<Text<'_>>()? {
                if key.0 == self.0 {
                    found = Some(map.next_value()?);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(found)
        }
    }
    serde_json::Deserializer::from_str(raw.get())
        .deserialize_map(Field(name))
        .ok()
        .flatten()
}
