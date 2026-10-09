//! 有界 JSON 字段选择器，仅支持成员、索引和单层通配

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Segment {
    Key(String),
    Index(usize),
    Wildcard,
}

pub(super) fn parse(source: &str) -> Result<Vec<Segment>, &'static str> {
    if source.len() > 1024 || !source.starts_with('$') {
        return Err("字段路径应以 $ 开头且不超过 1024 字节");
    }
    let mut rest = &source[1..];
    let mut segments = Vec::new();
    while !rest.is_empty() {
        if segments.len() == 16 {
            return Err("字段路径最多 16 层");
        }
        if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(['.', '[']).unwrap_or(tail.len());
            let key = &tail[..end];
            segments.push(if key == "*" {
                Segment::Wildcard
            } else if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                Segment::Key(key.to_owned())
            } else {
                return Err("成员名包含特殊字符时使用双引号键名");
            });
            rest = &tail[end..];
        } else if let Some(tail) = rest.strip_prefix('[') {
            if tail.starts_with('"') {
                let mut decoder = serde_json::Deserializer::from_str(tail).into_iter::<String>();
                let key = decoder
                    .next()
                    .and_then(Result::ok)
                    .ok_or("无效的双引号键名")?;
                rest = tail[decoder.byte_offset()..]
                    .strip_prefix(']')
                    .ok_or("字段路径缺少 ]")?;
                segments.push(Segment::Key(key));
            } else {
                let (value, tail) = tail.split_once(']').ok_or("字段路径缺少 ]")?;
                segments.push(if value == "*" {
                    Segment::Wildcard
                } else if !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) {
                    Segment::Index(value.parse().map_err(|_| "数组索引超出范围")?)
                } else {
                    return Err("仅支持数组索引和 * 通配，不支持过滤表达式");
                });
                rest = tail;
            }
        } else {
            return Err("无效的字段路径");
        }
    }
    Ok(segments)
}

/// 从后向前访问数组，删除匹配项不会改变尚未访问的索引
pub(super) fn visit(
    value: &mut Value,
    segments: &[Segment],
    budget: &mut super::Budget<'_>,
    apply: &mut impl FnMut(&mut Value, &mut super::Budget<'_>) -> Result<bool, &'static str>,
) -> Result<bool, &'static str> {
    budget.step()?;
    let Some((first, rest)) = segments.split_first() else {
        return apply(value, budget);
    };
    match (first, value) {
        (Segment::Key(key), Value::Object(object)) => {
            if let Some(child) = object.get_mut(key)
                && visit(child, rest, budget, apply)?
            {
                object.remove(key);
            }
        }
        (Segment::Index(index), Value::Array(items)) => {
            if let Some(child) = items.get_mut(*index)
                && visit(child, rest, budget, apply)?
            {
                items.remove(*index);
            }
        }
        (Segment::Wildcard, Value::Object(object)) => {
            let keys: Vec<_> = object.keys().cloned().collect();
            for key in keys {
                if let Some(child) = object.get_mut(&key)
                    && visit(child, rest, budget, apply)?
                {
                    object.remove(&key);
                }
            }
        }
        (Segment::Wildcard, Value::Array(items)) => {
            for index in (0..items.len()).rev() {
                if visit(&mut items[index], rest, budget, apply)? {
                    items.remove(index);
                }
            }
        }
        // 缺少字段属于正常未命中，已存在却不符合路径类型属于错误
        _ => return Err("目标结构不符合字段路径"),
    }
    Ok(false)
}
