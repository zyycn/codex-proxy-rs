//! 在 attempt 中间件提交后应用所选平台的生成规则

use super::provider::ProviderMiddlewareTerminal;
use crate::{operation::Operation, settings::prompt_rewrite::ProviderPromptRules};
use std::sync::Arc;

pub(super) fn wrap(
    terminal: ProviderMiddlewareTerminal,
    rules: &Arc<ProviderPromptRules>,
) -> ProviderMiddlewareTerminal {
    let rules = Arc::clone(rules);
    Box::new(move |mut operation, headers| {
        if let Operation::Generate(request) = &mut operation {
            let payload = request.protocol_payload();
            if let Some(edit) =
                gateway_protocol::prompt_rewrite::plan(payload.protocol(), payload.body(), |text| {
                    rules.replace_prefix(text)
                })
            {
                request.apply_prompt_edit(edit);
            }
        }
        terminal(operation, headers)
    })
}
