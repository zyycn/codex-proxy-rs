//! 将自动 Git 上下文的客户端版本与本次请求画像同步，保留项目和会话状态

use serde_json::Value;

use super::{CodexWireProfile, selection::ClientKind};
use crate::automatic_context::{locate_contexts, rewrite_git_context};

impl CodexWireProfile {
    pub(crate) fn apply_context_versions(&self, body: &mut Value) {
        // 与隐私规则共享承载识别；不扫描普通对话、工具输出或嵌入的引用片段
        let Ok(targets) = locate_contexts(body, true, || Ok(())) else {
            return;
        };
        for target in targets {
            let Some(Value::String(text)) = body.pointer_mut(&target.path) else {
                continue;
            };
            if text.len() > 1024 * 1024 || !target.is_complete(text) {
                continue;
            }
            // 官方 Desktop 26.1007.71326 的 codex_git_state 两个版本字段均可为空
            // 未知或损坏上下文保持原样，显式隐私规则仍按自己的失败策略执行
            let _ = rewrite_git_context(text, 1024 * 1024, |value| {
                let mut changed = 0;
                for (field, version) in [
                    ("appServerVersion", Some(self.codex_version.as_str())),
                    (
                        "appVersion",
                        (self.client_kind == ClientKind::Desktop
                            && !self.desktop_version.is_empty())
                        .then_some(self.desktop_version.as_str()),
                    ),
                ] {
                    let Some(current) = value.get_mut(field) else {
                        continue;
                    };
                    if !(current.is_string() || current.is_null()) {
                        continue;
                    }
                    let next =
                        version.map_or(Value::Null, |version| Value::String(version.to_owned()));
                    if *current != next {
                        *current = next;
                        changed += 1;
                    }
                }
                Ok(changed)
            });
        }
    }
}
