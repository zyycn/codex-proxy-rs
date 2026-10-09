//! 单条规则的编译、捕获组校验和受预算约束的值修改

use std::collections::BTreeSet;

use gateway_core::settings::privacy::{PrivacyAction, PrivacyRule, PrivacyScope};
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use super::{Budget, MAX_VALUE_BYTES, selector};

pub(super) struct CompiledRule {
    pub(super) rule: PrivacyRule,
    pub(super) path: Vec<selector::Segment>,
    regex: Option<Regex>,
}

impl CompiledRule {
    pub(super) fn compile(rule: &PrivacyRule) -> Result<Self, &'static str> {
        if rule.id.is_empty() || rule.id.len() > 64 || rule.name.is_empty() || rule.name.len() > 192
        {
            return Err("规则 ID 或名称长度不合法");
        }
        if rule.replacement.len() > 4096
            || serde_json::to_vec(&rule.value).map_or(true, |v| v.len() > 4096)
        {
            return Err("替换内容最多 4096 字节");
        }
        let path = if rule.scope == PrivacyScope::RequestHeader {
            Vec::new()
        } else {
            selector::parse(&rule.selector)?
        };
        if rule.scope == PrivacyScope::RequestHeader {
            reqwest::header::HeaderName::from_bytes(rule.selector.as_bytes())
                .map_err(|_| "请求头名称无效")?;
            if rule.action == PrivacyAction::RenameKey {
                return Err("请求头范围不支持对象键名替换");
            }
        }
        if rule.scope == PrivacyScope::EnvironmentText && !path.is_empty() {
            return Err("环境文本使用 $ 选择整个文本值");
        }
        let regex = rule
            .pattern
            .as_ref()
            .map(|pattern| {
                if pattern.len() > 4096 {
                    return Err("正则表达式最多 4096 字节");
                }
                RegexBuilder::new(pattern)
                    .case_insensitive(rule.case_insensitive)
                    .multi_line(rule.multi_line)
                    .size_limit(1024 * 1024)
                    .dfa_size_limit(1024 * 1024)
                    .nest_limit(64)
                    .build()
                    .map_err(|_| "正则语法无效或复杂度超限，不支持前后顾与模式反向引用")
            })
            .transpose()?;
        if matches!(
            rule.action,
            PrivacyAction::RegexReplace | PrivacyAction::RenameKey
        ) {
            validate_replacement(&rule.replacement, regex.as_ref().ok_or("请填写匹配表达式")?)?;
        }
        if rule.action == PrivacyAction::SetValue && rule.pattern.is_some() {
            return Err("固定值动作不使用匹配表达式");
        }
        Ok(Self {
            rule: rule.clone(),
            path,
            regex,
        })
    }

    pub(super) fn apply(
        &self,
        value: &mut Value,
        budget: &mut Budget<'_>,
    ) -> Result<usize, &'static str> {
        let mut count = 0;
        let removed = selector::visit(value, &self.path, budget, &mut |value, budget| {
            self.apply_value(value, budget, &mut count)
        })?;
        if removed {
            return Err("不能删除作用范围根节点");
        }
        Ok(count)
    }

    pub(super) fn apply_value(
        &self,
        value: &mut Value,
        budget: &mut Budget<'_>,
        count: &mut usize,
    ) -> Result<bool, &'static str> {
        budget.step()?;
        match self.rule.action {
            PrivacyAction::RemoveField => {
                if let Some(regex) = &self.regex {
                    let text = value.as_str().ok_or("条件删除只能匹配字符串值")?;
                    budget.text(text)?;
                    if !regex.is_match(text) {
                        return Ok(false);
                    }
                }
                *count += 1;
                return Ok(true);
            }
            PrivacyAction::SetValue => {
                if std::mem::discriminant(value) != std::mem::discriminant(&self.rule.value) {
                    return Err("固定值必须保持原字段类型");
                }
                *value = self.rule.value.clone();
                *count += 1;
            }
            PrivacyAction::RegexReplace => {
                let text = value.as_str().ok_or("正则替换只能处理字符串")?;
                let (replacement, matches) = self.replace(text, budget)?;
                *value = Value::String(replacement);
                *count += matches;
            }
            PrivacyAction::RenameKey => {
                let object = value.as_object().ok_or("字段名替换只能处理对象")?;
                let mut keys = BTreeSet::new();
                let mut changes = Vec::new();
                for key in object.keys() {
                    budget.step()?;
                    let (replacement, matches) = self.replace(key, budget)?;
                    if !keys.insert(replacement.clone()) {
                        return Err("字段名替换产生重名");
                    }
                    *count += matches;
                    changes.push(replacement);
                }
                let Value::Object(object) = std::mem::take(value) else {
                    return Err("目标类型已变化");
                };
                *value = Value::Object(changes.into_iter().zip(object.into_values()).collect());
            }
        }
        Ok(false)
    }

    fn replace(
        &self,
        text: &str,
        budget: &mut Budget<'_>,
    ) -> Result<(String, usize), &'static str> {
        budget.text(text)?;
        let regex = self.regex.as_ref().ok_or("缺少已编译表达式")?;
        let mut output = String::new();
        let mut end = 0;
        let mut count = 0;
        for captures in regex.captures_iter(text) {
            budget.step()?;
            let matched = captures.get(0).ok_or("缺少匹配结果")?;
            output.push_str(&text[end..matched.start()]);
            let largest = captures
                .iter()
                .flatten()
                .map(|group| group.len())
                .max()
                .unwrap_or(0);
            let references = self
                .rule
                .replacement
                .bytes()
                .filter(|byte| *byte == b'$')
                .count();
            if output
                .len()
                .saturating_add(self.rule.replacement.len())
                .saturating_add(largest.saturating_mul(references))
                > MAX_VALUE_BYTES
            {
                return Err("捕获组展开超出大小限制");
            }
            captures.expand(&self.rule.replacement, &mut output);
            if output.len() > MAX_VALUE_BYTES {
                return Err("替换结果超出大小限制");
            }
            end = matched.end();
            count += 1;
            if !self.rule.replace_all {
                break;
            }
        }
        output.push_str(&text[end..]);
        if output.len() > MAX_VALUE_BYTES {
            return Err("替换结果超出大小限制");
        }
        Ok((output, count))
    }
}

fn validate_replacement(value: &str, regex: &Regex) -> Result<(), &'static str> {
    let mut rest = value;
    while let Some(index) = rest.find('$') {
        rest = &rest[index + 1..];
        if let Some(tail) = rest.strip_prefix('$') {
            rest = tail;
            continue;
        }
        let (name, tail) = if let Some(tail) = rest.strip_prefix('{') {
            tail.split_once('}').ok_or("捕获组引用缺少 }")?
        } else {
            let length = rest
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count();
            if length == 0 {
                continue;
            }
            (&rest[..length], &rest[length..])
        };
        let valid = name.parse::<usize>().map_or_else(
            |_| regex.capture_names().flatten().any(|group| group == name),
            |index| index < regex.captures_len(),
        );
        if !valid {
            return Err("替换内容引用了不存在的捕获组");
        }
        rest = tail;
    }
    Ok(())
}
