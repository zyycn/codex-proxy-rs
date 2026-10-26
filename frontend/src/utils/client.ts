import { API_BASE_URL } from '@/api/constants'

const CODEX_DEFAULT_MODEL = 'gpt-5.6-terra'
export const CODEX_WEBSOCKET_ENABLED_BY_DEFAULT = false

interface CodexConfigInput {
  apiKey: string
  baseUrl: string
  websocketEnabled?: boolean
}

export function buildCodexConfig(input: CodexConfigInput): string {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  const websocketEnabled = input.websocketEnabled ?? CODEX_WEBSOCKET_ENABLED_BY_DEFAULT
  return `model_provider = "OpenAI"
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
# 代理密钥仅用于网关鉴权，真实账号登录状态由服务端管理
experimental_bearer_token = ${JSON.stringify(input.apiKey)}

[model_providers.OpenAI.capabilities]
# Codex 0.162.0+ 显式启用 V2 远程压缩，不依赖 Provider 名称识别
remote_compaction = "v2"

[model_providers.OpenAI.http_headers]
# 声明服务端托管认证，让官方客户端启用原生生图，该标记不是密钥
X-OpenAI-Actor-Authorization = "proxy-managed"

[features]
image_generation = true
goals = true`
}

interface CodexCcSwitchImportInput {
  apiKey: string
  baseUrl: string
  providerName: string
}

function buildUsageScript(apiKey: string, baseUrl: string) {
  // 导入端的占位符直接替换 JS 源码，不能安全承载含引号的自定义 Key
  // 按本次导入值生成字符串字面量，并转义左花括号，避免值中的占位符被二次替换。
  const url = JSON.stringify(`${baseUrl}/usage`).replaceAll('{', '\\u007b')
  const authorization = JSON.stringify(`Bearer ${apiKey}`).replaceAll('{', '\\u007b')
  // 不限额时省略 total / remaining，避免把无限额度显示成余额为零。
  return `({
  request: {
    url: ${url},
    method: "GET",
    headers: { Authorization: ${authorization} }
  },
  extractor: function(response) {
    return [["daily", "日额度"], ["weekly", "周额度"]].map(function(entry) {
      var budget = response[entry[0]];
      var result = {
        planName: entry[1],
        isValid: true,
        used: Number(budget.used),
        unit: response.unit
      };
      if (budget.total === null) {
        result.extra = "不限额";
      } else {
        result.total = Number(budget.total);
        result.remaining = Number(budget.remaining);
      }
      return result;
    });
  }
})`
}

export function buildCodexCcSwitchImportDeeplink(input: CodexCcSwitchImportInput): string {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  // 导入端会重建 Codex 配置，只传连接信息和用量查询，不传无法保留的完整模板
  const entries: [string, string][] = [
    ['resource', 'provider'],
    ['app', 'codex'],
    ['model', CODEX_DEFAULT_MODEL],
    ['name', input.providerName],
    ['homepage', baseUrl],
    ['endpoint', baseUrl],
    ['apiKey', input.apiKey],
    ['usageEnabled', 'true'],
    [
      'usageScript',
      encodeBase64(buildUsageScript(input.apiKey, baseUrl)),
    ],
    ['usageAutoInterval', '30'],
  ]

  return `ccswitch://v1/import?${new URLSearchParams(entries).toString()}`
}

function encodeBase64(value: string) {
  const bytes = new TextEncoder().encode(value)
  let binary = ''
  for (const byte of bytes)
    binary += String.fromCharCode(byte)
  return btoa(binary)
}

export function resolveServiceRootUrl() {
  const normalizedApiBase = API_BASE_URL.trim().replace(/\/+$/, '')
  if (/^https?:\/\//i.test(normalizedApiBase))
    return normalizedApiBase
  if (typeof window === 'undefined')
    return normalizedApiBase

  const origin = window.location.origin.replace(/\/+$/, '')
  if (!normalizedApiBase)
    return origin
  return `${origin}${normalizedApiBase.startsWith('/')
    ? normalizedApiBase
    : `/${normalizedApiBase}`}`
}
