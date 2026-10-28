//! 账号错误保护的可编辑规则、匹配事实与有界恢复快照

use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

/// 规则只观察已解析的上游事实，不扫描原始响应或改写协议分类
#[derive(Debug, Clone, Copy)]
pub struct AccountErrorFacts<'a> {
    pub provider: &'a str,
    pub status: Option<u16>,
    pub code: Option<&'a str>,
    pub error_type: Option<&'a str>,
    pub message: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountErrorField {
    Provider,
    Status,
    Code,
    Type,
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountErrorOperator {
    Equals,
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountErrorCondition {
    pub field: AccountErrorField,
    pub operator: AccountErrorOperator,
    pub values: Vec<Option<String>>,
    #[serde(default)]
    pub ignore_case: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountErrorRule {
    pub id: String,
    pub name: String,
    pub conditions: Vec<AccountErrorCondition>,
}

/// 持久状态仅保存有界规则标识，不保存上游错误正文
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountProtectionMatch {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountErrorProtection {
    pub enabled: bool,
    pub rules: Vec<AccountErrorRule>,
    pub threshold: u32,
    pub window_seconds: u64,
    pub duration_seconds: u64,
    pub probe_enabled: bool,
    pub probe_model: Option<String>,
}

impl Default for AccountErrorProtection {
    fn default() -> Self {
        Self {
            enabled: false,
            rules: Vec::new(),
            threshold: 12,
            window_seconds: 600,
            duration_seconds: 7200,
            probe_enabled: true,
            probe_model: None,
        }
    }
}

impl AccountErrorProtection {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(2..=1000).contains(&self.threshold)
            || !(60..=3600).contains(&self.window_seconds)
            || !(300..=604800).contains(&self.duration_seconds)
            || self
                .probe_model
                .as_deref()
                .is_some_and(|v| !valid_label(v, 128))
            || self.rules.len() > 32
            || (self.enabled && self.rules.is_empty())
        {
            return Err("account_error_protection");
        }
        let mut text_bytes = self.probe_model.as_ref().map_or(0, String::len);
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            text_bytes += rule.id.len() + rule.name.len();
            if text_bytes > 65536 {
                return Err("account_error_protection");
            }
            if !valid_label(&rule.id, 128)
                || !valid_label(&rule.name, 128)
                || !ids.insert(&rule.id)
                || rule.conditions.is_empty()
                || rule.conditions.len() > 8
                || !rule
                    .conditions
                    .iter()
                    .any(|c| c.field != AccountErrorField::Provider)
            {
                return Err("account_error_protection");
            }
            for condition in &rule.conditions {
                let max = if condition.field == AccountErrorField::Message {
                    1024
                } else {
                    256
                };
                if condition.values.is_empty()
                    || condition.values.len() > 16
                    || (condition.operator == AccountErrorOperator::Contains
                        && condition.field != AccountErrorField::Message)
                {
                    return Err("account_error_protection");
                }
                for value in &condition.values {
                    text_bytes += value.as_ref().map_or(0, String::len);
                    if text_bytes > 65536 {
                        return Err("account_error_protection");
                    }
                    if value.as_deref().is_some_and(|v| v.chars().count() > max)
                        || (condition.operator == AccountErrorOperator::Contains
                            && value.as_deref().is_none_or(|v| v.trim().is_empty()))
                        || (condition.field == AccountErrorField::Status
                            && value.as_deref().is_some_and(|v| {
                                v.len() != 3
                                    || v.parse::<u16>()
                                        .map_or(true, |status| !(100..=599).contains(&status))
                            }))
                    {
                        return Err("account_error_protection");
                    }
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn compile(&self) -> CompiledAccountErrorProtection {
        let mut normalized = self.clone();
        let mut ignore_case = [false; 5];
        for rule in &mut normalized.rules {
            for condition in &mut rule.conditions {
                ignore_case[condition.field as usize] |=
                    condition.ignore_case && condition.operator == AccountErrorOperator::Contains;
                for candidate in condition.values.iter_mut().flatten() {
                    *candidate = if condition.ignore_case {
                        candidate.trim().to_ascii_lowercase()
                    } else {
                        candidate.trim().to_owned()
                    };
                }
            }
        }
        CompiledAccountErrorProtection {
            policy: self.clone(),
            fingerprint: self.fingerprint(),
            normalized,
            ignore_case,
        }
    }

    #[must_use]
    pub fn matching_rules(&self, facts: AccountErrorFacts<'_>) -> Vec<AccountProtectionMatch> {
        self.compile().matching_rules(facts)
    }

    /// 长度前缀避免分隔符碰撞，内容指纹用于保留无关设置更新前的计数
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        fn value(hash: &mut Sha256, value: &[u8]) {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        value(
            &mut hash,
            &[u8::from(self.enabled), u8::from(self.probe_enabled)],
        );
        value(&mut hash, &self.threshold.to_le_bytes());
        value(&mut hash, &self.window_seconds.to_le_bytes());
        value(&mut hash, &self.duration_seconds.to_le_bytes());
        value(
            &mut hash,
            self.probe_model.as_deref().unwrap_or_default().as_bytes(),
        );
        for rule in &self.rules {
            value(&mut hash, rule.id.as_bytes());
            value(&mut hash, rule.name.as_bytes());
            value(&mut hash, &(rule.conditions.len() as u64).to_le_bytes());
            for c in &rule.conditions {
                value(
                    &mut hash,
                    &[c.field as u8, c.operator as u8, u8::from(c.ignore_case)],
                );
                value(&mut hash, &(c.values.len() as u64).to_le_bytes());
                for v in &c.values {
                    value(&mut hash, &[u8::from(v.is_some())]);
                    value(&mut hash, v.as_deref().unwrap_or_default().as_bytes());
                }
            }
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.chars().count() <= max
        && !value.chars().any(char::is_control)
}

/// 发布时归一化候选并缓存指纹，请求只归一化每个实际输入一次
#[derive(Debug, Clone)]
pub struct CompiledAccountErrorProtection {
    policy: AccountErrorProtection,
    fingerprint: String,
    normalized: AccountErrorProtection,
    ignore_case: [bool; 5],
}

impl CompiledAccountErrorProtection {
    #[must_use]
    pub fn policy(&self) -> &AccountErrorProtection {
        &self.policy
    }
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    #[must_use]
    pub fn matching_rules(&self, facts: AccountErrorFacts<'_>) -> Vec<AccountProtectionMatch> {
        if !self.policy.enabled {
            return Vec::new();
        }
        let status = facts.status.map(|value| value.to_string());
        let raw = [
            Some(facts.provider),
            status.as_deref(),
            facts.code,
            facts.error_type,
            facts.message,
        ];
        // 保留实际存在性；超长输入不能冒充缺失，也不能用截断前缀做整串相等
        let full: [Option<&str>; 5] = raw.map(|value| value.map(str::trim));
        let limits = [1024, 3, 1024, 1024, 8192];
        let observed: [Option<&str>; 5] =
            std::array::from_fn(|index| raw[index].map(|text| prefix(text, limits[index]).trim()));
        let folded: [Option<String>; 5] = std::array::from_fn(|index| {
            if self.ignore_case[index] {
                observed[index].map(str::to_ascii_lowercase)
            } else {
                None
            }
        });
        self.normalized
            .rules
            .iter()
            .filter(|rule| {
                !rule.conditions.is_empty()
                    && rule.conditions.iter().all(|condition| {
                        let index = condition.field as usize;
                        let actual = if condition.ignore_case
                            && condition.operator == AccountErrorOperator::Contains
                        {
                            folded[index].as_deref()
                        } else {
                            observed[index]
                        };
                        condition.values.iter().any(|candidate| {
                            match (actual, candidate.as_deref()) {
                                (None, None) => condition.operator == AccountErrorOperator::Equals,
                                (Some(actual), Some(candidate)) => match condition.operator {
                                    AccountErrorOperator::Equals => {
                                        full[index].is_some_and(|value| {
                                            if condition.ignore_case {
                                                value.eq_ignore_ascii_case(candidate)
                                            } else {
                                                value == candidate
                                            }
                                        })
                                    }
                                    AccountErrorOperator::Contains => {
                                        !candidate.is_empty() && actual.contains(candidate)
                                    }
                                },
                                _ => false,
                            }
                        })
                    })
            })
            .map(|rule| AccountProtectionMatch {
                id: rule.id.clone(),
                name: rule.name.clone(),
            })
            .collect()
    }
}

fn prefix(value: &str, maximum: usize) -> &str {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
