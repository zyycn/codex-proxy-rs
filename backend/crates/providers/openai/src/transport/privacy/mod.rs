//! OpenAI 隐私规则的统一编译与执行，供设置预览和实际出站共享

mod carrier;
mod rule;
mod selector;

use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

use gateway_core::settings::privacy::{
    CodexPrivacyPolicy, CompiledPrivacyPolicy, PrivacyError, PrivacyFailureMode,
    PrivacyPreviewRequest, PrivacyPreviewResult, PrivacyRuleOutcome,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

const MAX_VALUE_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 256 * 1024;

struct Policy {
    enabled: bool,
    failure: PrivacyFailureMode,
    rules: Vec<rule::CompiledRule>,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledCodexPrivacyPolicy")
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

struct Budget<'a> {
    started: Instant,
    steps: usize,
    text_bytes: usize,
    cancelled: &'a dyn Fn() -> bool,
}

impl Budget<'_> {
    fn step(&mut self) -> Result<(), &'static str> {
        self.steps += 1;
        if (self.cancelled)() {
            return Err("请求已取消或超时");
        }
        if self.steps > 10_000 || self.started.elapsed() > Duration::from_millis(100) {
            return Err("规则执行预算超限");
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
    let rules = policy
        .rules
        .iter()
        .enumerate()
        .map(|(rule_index, rule)| {
            if !ids.insert(&rule.id) {
                return Err(PrivacyError {
                    rule_index,
                    reason: "规则 ID 重复",
                });
            }
            rule::CompiledRule::compile(rule).map_err(|reason| PrivacyError { rule_index, reason })
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
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<PrivacyRuleOutcome>, PrivacyError> {
        let mut outcomes = Vec::new();
        let mut budget = Budget {
            started: Instant::now(),
            steps: 0,
            text_bytes: 0,
            cancelled,
        };
        for (rule_index, rule) in self.rules.iter().enumerate() {
            let mut outcome = PrivacyRuleOutcome {
                rule_id: rule.rule.id.clone(),
                matches: 0,
                status: "disabled".to_owned(),
                reason: None,
            };
            if self.enabled && rule.rule.enabled {
                // 一条规则的所有承载副本同成同败，失败不会留下部分改写
                let result = (|| {
                    budget.step()?;
                    check_size(body, MAX_REQUEST_BYTES)?;
                    let mut next_body = body.clone();
                    let mut next_headers = headers.clone();
                    let mut next_metadata = turn_metadata.clone();
                    let count = carrier::apply(
                        rule,
                        &mut next_body,
                        &mut next_headers,
                        &mut next_metadata,
                        &mut budget,
                    )?;
                    check_size(&next_body, MAX_REQUEST_BYTES)?;
                    budget.step()?;
                    *body = next_body;
                    *headers = next_headers;
                    *turn_metadata = next_metadata;
                    Ok(count)
                })();
                match result {
                    Ok(count) => {
                        outcome.matches = count;
                        outcome.status =
                            if count == 0 { "unmatched" } else { "applied" }.to_owned();
                    }
                    Err(reason) => {
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
    let outcomes =
        compile(&request.policy)?.apply(&mut body, &mut headers, &mut turn_metadata, &|| false)?;
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
    serde_json::to_writer(Counter(limit), value).map_err(|_| "请求或预览样本超出大小限制")
}

impl super::client::CodexBackendClient {
    pub(crate) fn with_privacy(
        mut self,
        policy: Option<Arc<dyn CompiledPrivacyPolicy>>,
        cancellation: gateway_core::lifecycle::CancellationToken,
    ) -> Self {
        self.privacy = policy.map(|policy| (policy, cancellation));
        self
    }

    /// 所有宿主头和 WS 投影完成后再执行，配置者可改写最终业务字段
    pub(super) fn apply_privacy(
        &self,
        body: &mut Value,
        headers: &mut HeaderMap,
    ) -> Result<(), super::client::CodexClientError> {
        if let Some((policy, cancellation)) = &self.privacy {
            let outcomes =
                policy.apply(body, headers, &mut None, &|| cancellation.is_cancelled())?;
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
