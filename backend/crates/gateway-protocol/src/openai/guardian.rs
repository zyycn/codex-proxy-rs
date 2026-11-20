//! 在 Responses 入站身份字段合并前识别 Guardian 的三种调用

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

const CONTEXT_KEY: &str = "codex_guardian_identity";
/// Codex 原生 OAuth 同步审核使用的专用模型
pub const CODEX_AUTO_REVIEW_MODEL: &str = "codex-auto-review";

/// 当前 Responses 请求的 Codex Guardian 调用类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexGuardianRole {
    Reviewer,
    Classifier,
    Housekeeper,
}

impl CodexGuardianRole {
    /// 该调用在原生 OAuth 上游使用的角色头值
    #[must_use]
    pub fn header(self) -> Option<&'static str> {
        match self {
            Self::Reviewer => Some("reviewer"),
            Self::Classifier => Some("classifier"),
            Self::Housekeeper => None,
        }
    }

    /// 分类器和后台整理器用各自的 sampler 选择 WebSocket 或 HTTP
    #[must_use]
    pub fn uses_sampler_transport(self) -> bool {
        matches!(self, Self::Classifier | Self::Housekeeper)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Identity {
    Role(CodexGuardianRole),
    Incomplete,
    Other,
    Ambiguous,
}

impl Identity {
    fn label(self) -> &'static str {
        match self {
            Self::Role(CodexGuardianRole::Reviewer) => "reviewer",
            Self::Role(CodexGuardianRole::Classifier) => "classifier",
            Self::Role(CodexGuardianRole::Housekeeper) => "housekeeper",
            Self::Incomplete => "incomplete",
            Self::Other => "other",
            Self::Ambiguous => "ambiguous",
        }
    }
}

struct Source<'a> {
    subagent: Option<&'a str>,
    turn: Turn,
}

#[derive(Default, Deserialize)]
struct Turn {
    #[serde(default, deserialize_with = "present_string")]
    thread_source: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    turn_trigger: Option<String>,
}

// 区分缺失与错误类型；serde 对重复出现的身份字段也会拒绝
fn present_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

impl<'a> Source<'a> {
    fn new(subagent: Option<&'a str>, turn: Option<&str>) -> Option<Self> {
        let turn = match turn {
            Some(raw) if raw.trim_start().starts_with('{') => serde_json::from_str(raw).ok()?,
            Some(_) => return None,
            None => Turn::default(),
        };
        Some(Self { subagent, turn })
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        [
            self.subagent.zip(other.subagent),
            self.turn
                .thread_source
                .as_deref()
                .zip(other.turn.thread_source.as_deref()),
            self.turn
                .turn_trigger
                .as_deref()
                .zip(other.turn.turn_trigger.as_deref()),
        ]
        .into_iter()
        .flatten()
        .any(|(left, right)| left != right)
    }

    fn identity(&self) -> Identity {
        let source = self.turn.thread_source.as_deref();
        let trigger = self.turn.turn_trigger.as_deref();
        if self.subagent.is_some_and(|v| v != "guardian") {
            return Identity::Other;
        }
        let role = match (source, trigger) {
            (Some("guardian_review"), Some("guardian_classifier" | "guardian_housekeeper"))
            | (Some("guardian_classifier"), Some("guardian_review" | "guardian_housekeeper")) => {
                return Identity::Ambiguous;
            }
            (Some("guardian_review"), _) => CodexGuardianRole::Reviewer,
            (Some("guardian_classifier"), _) => CodexGuardianRole::Classifier,
            (None, Some("guardian_housekeeper")) => CodexGuardianRole::Housekeeper,
            (Some(_), _) => return Identity::Other,
            _ => return Identity::Incomplete,
        };
        if self.subagent.is_some() {
            Identity::Role(role)
        } else {
            Identity::Incomplete
        }
    }
}

fn identity(body: &Map<String, Value>, context: &Map<String, Value>) -> Identity {
    let metadata = body.get("client_metadata");
    let body_subagent = metadata.and_then(|m| m.get("x-openai-subagent"));
    let websocket = context
        .get(super::WEBSOCKET_FRAME_CONTEXT_KEY)
        .and_then(Value::as_bool)
        == Some(true);
    let header_subagent = (!websocket)
        .then(|| context.get("subagent"))
        .flatten()
        .and_then(Value::as_str);
    if metadata.is_some_and(|m| !m.is_object()) || body_subagent.is_some_and(|v| !v.is_string()) {
        return Identity::Ambiguous;
    }
    if body_subagent.and_then(Value::as_str) != Some("guardian")
        && header_subagent != Some("guardian")
    {
        return if body_subagent.is_some() || header_subagent.is_some() {
            Identity::Other
        } else {
            Identity::Incomplete
        };
    }
    let turn = metadata.and_then(|m| m.get("x-codex-turn-metadata"));
    if turn.is_some_and(|v| !v.is_string()) {
        return Identity::Ambiguous;
    }
    let Some(body) = Source::new(
        body_subagent.and_then(Value::as_str),
        turn.and_then(Value::as_str),
    ) else {
        return Identity::Ambiguous;
    };
    if websocket {
        return body.identity();
    }
    let Some(header) = Source::new(
        header_subagent,
        context.get("turn_metadata").and_then(Value::as_str),
    ) else {
        return Identity::Ambiguous;
    };
    if body.conflicts_with(&header) {
        return Identity::Ambiguous;
    }
    match (body.identity(), header.identity()) {
        (Identity::Ambiguous, _) | (_, Identity::Ambiguous) => Identity::Ambiguous,
        (Identity::Incomplete, header) => header,
        (body, _) => body,
    }
}

/// WS 只使用本帧正文；HTTP 两侧须各自完整，不能拼接彼此欠缺的身份字段
pub fn capture_codex_guardian_identity(
    body: &Map<String, Value>,
    context: &mut Map<String, Value>,
    guardian_header: Option<&str>,
    ambiguous_headers: bool,
) {
    if guardian_header.is_none()
        && !ambiguous_headers
        && context.get("subagent").and_then(Value::as_str) != Some("guardian")
        && body
            .get("client_metadata")
            .and_then(|m| m.get("x-openai-subagent"))
            .and_then(Value::as_str)
            != Some("guardian")
    {
        return;
    }
    let websocket = context
        .get(super::WEBSOCKET_FRAME_CONTEXT_KEY)
        .and_then(Value::as_bool)
        == Some(true);
    let mut found = identity(body, context);
    if !websocket
        && (ambiguous_headers
            || matches!((guardian_header, found),
        (Some(header), Identity::Role(role)) if role.header() != Some(header)))
    {
        found = Identity::Ambiguous;
    }
    context.insert(CONTEXT_KEY.into(), Value::from(found.label()));
}

/// 使用解码阶段保存的原始身份；程序内构造的请求可以直接使用正文和当前请求上下文
#[must_use]
pub fn codex_guardian_role(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
) -> Option<CodexGuardianRole> {
    if let Some(prepared) = context.get(CONTEXT_KEY) {
        return match prepared.as_str()? {
            "reviewer" => Some(CodexGuardianRole::Reviewer),
            "classifier" => Some(CodexGuardianRole::Classifier),
            "housekeeper" => Some(CodexGuardianRole::Housekeeper),
            _ => None,
        };
    }
    match identity(body, context) {
        Identity::Role(role) => Some(role),
        _ => None,
    }
}
