//! 按目标上游索引规则，共享配置原文，按配置顺序匹配前缀

use crate::identity::ProviderKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptRewriteRule {
    #[serde(default)]
    providers: BTreeSet<ProviderKind>,
    #[serde(deserialize_with = "nonempty_pattern")]
    pattern: String,
    replacement: String,
}

impl fmt::Debug for PromptRewriteRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptRewriteRule")
            .field("providers", &self.providers)
            .finish_non_exhaustive()
    }
}

fn nonempty_pattern<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let pattern = String::deserialize(de)?;
    if pattern.is_empty() {
        return Err(serde::de::Error::custom("匹配文本不能为空"));
    }
    Ok(pattern)
}

#[derive(Debug)]
pub struct ProviderPromptRules {
    rules: Arc<[PromptRewriteRule]>,
    global: Arc<BTreeSet<usize>>,
    scoped: BTreeSet<usize>,
}

impl ProviderPromptRules {
    pub fn replace_prefix(&self, text: &str) -> Option<String> {
        let (rule, suffix) = self
            .global
            .union(&self.scoped)
            .map(|&index| &self.rules[index])
            .find_map(|rule| {
                text.strip_prefix(&rule.pattern)
                    .map(|suffix| (rule, suffix))
            })?;
        if rule.pattern == rule.replacement {
            return None;
        }
        let mut updated = String::with_capacity(rule.replacement.len() + suffix.len());
        updated.push_str(&rule.replacement);
        updated.push_str(suffix);
        Some(updated)
    }
}

#[derive(Debug, Default)]
pub struct CompiledPromptRewrite {
    all: Option<Arc<ProviderPromptRules>>,
    by_provider: BTreeMap<ProviderKind, Arc<ProviderPromptRules>>,
}

impl CompiledPromptRewrite {
    pub fn new(rules: Arc<[PromptRewriteRule]>) -> Self {
        let mut global = BTreeSet::new();
        let mut scoped = BTreeMap::<_, BTreeSet<_>>::new();
        for (index, rule) in rules.iter().enumerate() {
            if rule.providers.is_empty() {
                global.insert(index);
            } else {
                for provider in &rule.providers {
                    scoped.entry(provider).or_default().insert(index);
                }
            }
        }
        let global = Arc::new(global);
        let selection = |scoped| {
            Arc::new(ProviderPromptRules {
                rules: Arc::clone(&rules),
                global: Arc::clone(&global),
                scoped,
            })
        };
        Self {
            all: (!global.is_empty()).then(|| selection(BTreeSet::new())),
            by_provider: scoped
                .into_iter()
                .map(|(id, indices)| (id.clone(), selection(indices)))
                .collect(),
        }
    }

    pub fn for_provider(&self, provider: &ProviderKind) -> Option<Arc<ProviderPromptRules>> {
        self.by_provider
            .get(provider)
            .or(self.all.as_ref())
            .cloned()
    }
}
