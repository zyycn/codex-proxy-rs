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
  const auth = { OPENAI_API_KEY: input.apiKey }
  const configToml = `model_provider = "OpenAI"
model = "${CODEX_DEFAULT_MODEL}"
review_model = "${CODEX_DEFAULT_MODEL}"
model_reasoning_effort = "max"
service_tier = "default"
disable_response_storage = true
network_access = "enabled"

[model_providers.OpenAI]
name = "OpenAI"
base_url = "${baseUrl}"
wire_api = "responses"${websocketEnabled ? '\nsupports_websockets = true' : ''}
requires_openai_auth = true

[features]${websocketEnabled ? '\nresponses_websockets_v2 = true' : ''}
goals = true`

  return {
    auth,
    authJson: JSON.stringify(auth, null, 2),
    baseUrl,
    configToml,
  }
}
