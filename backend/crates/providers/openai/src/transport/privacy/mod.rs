//! OpenAI 隐私规则的统一编译与执行，供设置预览和实际出站共享

mod carrier;
mod changes;
mod rule;
mod selector;
mod workspace;

use std::{collections::BTreeSet, sync::Arc};

use gateway_core::settings::privacy::{
    CodexPrivacyPolicy, CompiledPrivacyPolicy, PrivacyError, PrivacyFailureMode,
    PrivacyPreviewRequest, PrivacyPreviewResult, PrivacyRuleOutcome, PrivacyScope,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

const MAX_VALUE_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 256 * 1024;

struct Policy {
    enabled: bool,
    failure: PrivacyFailureMode,
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    id: String,
    enabled: bool,
    steps: Vec<rule::CompiledStep>,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledCodexPrivacyPolicy")
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

struct Budget<'a> {
    steps: usize,
    text_bytes: usize,
    session_scope: Option<&'a str>,
    cancelled: &'a dyn Fn() -> bool,
}

impl Budget<'_> {
    fn step(&mut self) -> Result<(), &'static str> {
        self.steps += 1;
        if (self.cancelled)() {
            return Err("请求已取消或超时");
        }
        if self.steps > 10_000 {
            return Err("规则执行步数超限");
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), &'static str> {
        self.step()?;
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        if text.len() > MAX_VALUE_BYTES || self.text_bytes > 16 * MAX_VALUE_BYTES {
            return Err("目标文本超出大小限制");
        }
        Ok(())
    }
}

pub fn compile(
    policy: &CodexPrivacyPolicy,
) -> Result<Arc<dyn CompiledPrivacyPolicy>, PrivacyError> {
    if policy.rules.len() > 32 {
        return Err(PrivacyError {
            rule_index: 0,
            reason: "最多配置 32 条规则",
        });
    }
    let mut ids = BTreeSet::new();
    let mut step_count = 0;
    let rules = policy
        .rules
        .iter()
        .enumerate()
        .map(|(rule_index, rule)| {
            if rule.id.is_empty()
                || rule.id.len() > 64
                || rule.name.is_empty()
                || rule.name.len() > 192
            {
                return Err(PrivacyError {
                    rule_index,
                    reason: "规则 ID 或名称长度不合法",
                });
            }
            if !ids.insert(&rule.id) {
                return Err(PrivacyError {
                    rule_index,
                    reason: "规则 ID 重复",
                });
            }
            step_count += rule.steps.len();
            if rule.steps.is_empty() || step_count > 32 {
                return Err(PrivacyError {
                    rule_index,
                    reason: "每条规则至少一个处理步骤，所有规则合计最多 32 个步骤",
                });
            }
            let steps = rule
                .steps
                .iter()
                .map(rule::CompiledStep::compile)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|reason| PrivacyError { rule_index, reason })?;
            Ok(CompiledRule {
                id: rule.id.clone(),
                enabled: rule.enabled,
                steps,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(Policy {
        enabled: policy.enabled,
        failure: policy.on_error,
        rules,
    }))
}

impl CompiledPrivacyPolicy for Policy {
    fn apply(
        &self,
        body: &mut Value,
        headers: &mut HeaderMap,
        turn_metadata: &mut Option<String>,
        session_scope: Option<&str>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<PrivacyRuleOutcome>, PrivacyError> {
        let mut outcomes = Vec::new();
        let mut budget = Budget {
            steps: 0,
            text_bytes: 0,
            session_scope,
            cancelled,
        };
        let mut body_size = None;
        let mut contexts = carrier::ContextIndex::default();
        for (rule_index, rule) in self.rules.iter().enumerate() {
            let mut outcome = PrivacyRuleOutcome {
                rule_id: rule.id.clone(),
                matches: 0,
                status: "disabled".to_owned(),
                reason: None,
            };
            if self.enabled && rule.enabled {
                // 一条规则的所有步骤与承载副本同成同败，失败不会留下部分改写
                let result = (|| {
                    budget.step()?;
                    let original_size =
                        *body_size.get_or_insert_with(|| encoded_size(body, MAX_REQUEST_BYTES));
                    let original_size = original_size?;
                    let mut next_headers = headers.clone();
                    let mut next_metadata = turn_metadata.clone();
                    let mut transaction = changes::BodyTransaction::new(body);
                    let mut next_size = original_size;
                    let mut count = 0;
                    for step in &rule.steps {
                        budget.step()?;
                        let mut changes = changes::BodyChanges::new(next_size);
                        let matches = carrier::prepare(
                            step,
                            transaction.body,
                            &mut next_headers,
                            &mut next_metadata,
                            &mut changes,
                            &mut contexts,
                            &mut budget,
                        )?;
                        next_size = changes.size()?;
                        budget.step()?;
                        changes.commit(&mut transaction);
                        count += matches;
                        if matches > 0 && step.rule.scope == PrivacyScope::RequestBody {
                            contexts = carrier::ContextIndex::default();
                        }
                    }
                    budget.step()?;
                    transaction.finish();
                    *headers = next_headers;
                    *turn_metadata = next_metadata;
                    body_size = Some(Ok(next_size));
                    Ok(count)
                })();
                match result {
                    Ok(count) => {
                        outcome.matches = count;
                        outcome.status =
                            if count == 0 { "unmatched" } else { "applied" }.to_owned();
                    }
                    Err(reason) => {
                        // 索引可能来自已回滚的中间正文，不能复用到下一条规则
                        contexts = carrier::ContextIndex::default();
                        if self.failure == PrivacyFailureMode::RejectRequest || cancelled() {
                            return Err(PrivacyError { rule_index, reason });
                        }
                        outcome.status = "skipped".to_owned();
                        outcome.reason = Some(reason.to_owned());
                    }
                }
            }
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
}

pub fn preview(request: PrivacyPreviewRequest) -> Result<PrivacyPreviewResult, PrivacyError> {
    check_size(&request.body, MAX_PREVIEW_BYTES).map_err(|reason| PrivacyError {
        rule_index: 0,
        reason,
    })?;
    let size = request
        .headers
        .iter()
        .map(|(key, values)| key.len() + values.iter().map(String::len).sum::<usize>())
        .sum::<usize>()
        + request.turn_metadata.as_ref().map_or(0, String::len);
    if size > MAX_PREVIEW_BYTES {
        return Err(PrivacyError {
            rule_index: 0,
            reason: "预览样本最多 256 KiB",
        });
    }
    let mut headers = HeaderMap::new();
    for (key, values) in request.headers {
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|_| PrivacyError {
            rule_index: 0,
            reason: "预览请求头名称无效",
        })?;
        for value in values {
            headers.append(
                name.clone(),
                HeaderValue::from_str(&value).map_err(|_| PrivacyError {
                    rule_index: 0,
                    reason: "预览请求头值无效",
                })?,
            );
        }
    }
    let mut body = request.body;
    let mut turn_metadata = request.turn_metadata;
    let session = workspace::preview_session(&body, &headers, turn_metadata.as_deref());
    let outcomes = compile(&request.policy)?.apply(
        &mut body,
        &mut headers,
        &mut turn_metadata,
        session.as_deref(),
        &|| false,
    )?;
    let headers = headers
        .keys()
        .map(|key| {
            (
                key.as_str().to_owned(),
                headers
                    .get_all(key)
                    .iter()
                    .filter_map(|value| {
                        std::str::from_utf8(value.as_bytes())
                            .ok()
                            .map(str::to_owned)
                    })
                    .collect(),
            )
        })
        .collect();
    Ok(PrivacyPreviewResult {
        body,
        headers,
        turn_metadata,
        outcomes,
    })
}

fn check_size(value: &Value, limit: usize) -> Result<(), &'static str> {
    encoded_size(value, limit).map(|_| ())
}

fn encoded_size(value: &Value, limit: usize) -> Result<usize, &'static str> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("size limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(limit);
    serde_json::to_writer(&mut counter, value).map_err(|_| "请求或预览样本超出大小限制")?;
    Ok(limit - counter.0)
}

impl super::client::CodexBackendClient {
    /// 使用账号隔离流程选定的同一 lease 冻结隐私作用域，不从可改写的出站字段识别换号
    pub(crate) fn with_account_privacy(
        mut self,
        policy: Option<Arc<dyn CompiledPrivacyPolicy>>,
        cancellation: gateway_core::lifecycle::CancellationToken,
        account: &gateway_core::account::ProviderAccount,
        session_scope: Option<&str>,
    ) -> Self {
        self.privacy = policy.map(|policy| {
            let scope = session_scope
                .filter(|session| !session.trim().is_empty())
                .map(|session| workspace::account_session_scope(session, account));
            (policy, cancellation, scope)
        });
        self
    }

    /// 所有宿主头和 WS 投影完成后再执行，配置者可改写最终业务字段
    pub(super) fn apply_privacy(
        &self,
        body: &mut Value,
        headers: &mut HeaderMap,
    ) -> Result<(), super::client::CodexClientError> {
        if let Some((policy, cancellation, session_scope)) = &self.privacy {
            let outcomes =
                policy.apply(body, headers, &mut None, session_scope.as_deref(), &|| {
                    cancellation.is_cancelled()
                })?;
            for (index, outcome) in outcomes
                .iter()
                .enumerate()
                .filter(|(_, outcome)| outcome.status == "skipped")
            {
                tracing::warn!(
                    rule_index = index,
                    reason = outcome.reason,
                    "隐私规则未生效，按配置跳过"
                );
            }
        }
        Ok(())
    }
}
