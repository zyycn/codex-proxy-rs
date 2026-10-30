//! 当前逻辑响应的中断意图、无控制通道时的拒绝与跨重试转发

use std::{borrow::Cow, collections::BTreeSet};

use gateway_core::engine::response_control::{ResponseControl, ResponseControlBinding};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct EventIdentity<'a> {
    #[serde(rename = "type", borrow)]
    event_type: Cow<'a, str>,
    #[serde(borrow)]
    response: Option<ResponseIdentity<'a>>,
}

#[derive(Deserialize)]
struct ResponseIdentity<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
}

#[derive(Default)]
pub(super) struct ResponseInterrupt {
    delivered_ids: BTreeSet<String>,
    current: Option<(String, ResponseControlBinding)>,
    pending: Option<Value>,
    sent: bool,
}

impl ResponseInterrupt {
    pub(super) fn created_id(payload: &str) -> Option<String> {
        let event: EventIdentity<'_> = serde_json::from_str(payload).ok()?;
        (event.event_type == "response.created").then_some(())?;
        Some(event.response?.id.into_owned())
    }

    /// 只在 created 实际写入下游后更新，不能把未交付的 ID 当成客户端目标
    pub(super) fn observe_created(&mut self, id: String, control: &ResponseControl) {
        let Some(binding) = control.binding() else {
            return;
        };
        if self
            .current
            .as_ref()
            .is_none_or(|(previous, old)| previous != &id || !old.same_binding(&binding))
        {
            self.sent = false;
        }
        self.delivered_ids.insert(id.clone());
        self.current = Some((id, binding));
    }

    /// 未知控制消息、模式和其他逻辑响应的 ID 仍走原始透明通道
    pub(super) fn retain(&mut self, payload: &str) -> bool {
        let Some(event) = interrupt_request(payload) else {
            return false;
        };
        if !event["response_id"]
            .as_str()
            .is_some_and(|id| self.delivered_ids.contains(id))
        {
            return false;
        }
        self.pending = Some(event);
        self.sent = false;
        true
    }

    /// 中断失败不代表模型响应失败，客户端仍可收完真实终态后续接
    pub(super) fn unavailable_event(payload: &str) -> Option<String> {
        let event = interrupt_request(payload)?;
        Some(
            serde_json::json!({
                "type": "response.interrupt.failed",
                "response_id": event["response_id"],
            })
            .to_string(),
        )
    }

    pub(super) async fn forward_pending(&mut self, control: &ResponseControl) {
        if self.sent {
            return;
        }
        let (Some(payload), Some((id, binding))) = (&mut self.pending, &self.current) else {
            return;
        };
        // 换号已绑定、created 尚未交付时等待新 ID；连接快照保证 await 期间也不会串号
        if !control
            .binding()
            .is_some_and(|current| binding.same_binding(&current))
        {
            return;
        }
        payload["response_id"] = Value::String(id.clone());
        if binding.send(&payload.to_string()).await.is_ok() {
            self.sent = true;
        }
    }
}

fn interrupt_request(payload: &str) -> Option<Value> {
    let event: Value = serde_json::from_str(payload).ok()?;
    (event["type"] == "response.interrupt"
        && event["mode"] == "discard_partial_items"
        && event["response_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()))
    .then_some(event)
}
