export const CODEX_DEFAULT_MODEL = 'gpt-5.6-terra'
export const CODEX_WEBSOCKET_ENABLED_BY_DEFAULT = false

export interface CodexConfigInput {
  apiKey: string
  baseUrl: string
  websocketEnabled?: boolean
}

export function buildCodexConfigFiles(input: CodexConfigInput) {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  const websocketEnabled = input.websocketEnabled ?? CODEX_WEBSOCKET_ENABLED_BY_DEFAULT
  // 保留 auth.json 载荷，兼容仍依赖该字段的 CCSwitch 导入器。
  const auth = { OPENAI_API_KEY: input.apiKey }
  const configToml = `model_provider = "OpenAI"
model = "${CODEX_DEFAULT_MODEL}"
review_model = "${CODEX_DEFAULT_MODEL}"
model_reasoning_effort = "max"
service_tier = "default"

[model_providers.OpenAI]
name = "OpenAI"
base_url = ${JSON.stringify(baseUrl)}
wire_api = "responses"
supports_websockets = ${websocketEnabled}
requires_openai_auth = false
# 代理密钥仅用于网关鉴权，真实账号登录状态由服务端管理。
experimental_bearer_token = ${JSON.stringify(input.apiKey)}

[model_providers.OpenAI.http_headers]
# 声明服务端托管认证，让官方客户端启用原生生图；该标记不是密钥。
X-OpenAI-Actor-Authorization = "proxy-managed"

[features]
image_generation = true
goals = true`

  return {
    auth,
    authJson: JSON.stringify(auth, null, 2),
    baseUrl,
    configToml,
  }
}
