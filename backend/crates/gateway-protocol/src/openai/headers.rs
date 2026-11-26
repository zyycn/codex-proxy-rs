//! OpenAI 与 Codex 响应中的账号身份字段分类

/// 账号与安装身份不能随上游响应下发给客户端
#[must_use]
pub fn is_private_response_header(name: &str) -> bool {
    let name = name.trim();
    [
        "chatgpt-account-id",
        "chatgpt-organization-id",
        "chatgpt-org-id",
        "chatgpt-project-id",
        "openai-organization",
        "openai-project",
        "x-openai-organization",
        "x-openai-project",
        "x-codex-installation-id",
        "x-codex-turn-metadata",
    ]
    .iter()
    .any(|private| name.eq_ignore_ascii_case(private))
}
