import type { BuiltinProvider } from '@/providers/catalog'
import { API_BASE_URL } from '@/api/constants'

const clientPresets = {
  openai: { app: 'codex', model: 'gpt-6-luna' },
  xai: { app: 'grokbuild', model: 'grok-4.7' },
  antigravity: { app: 'agy', model: 'Gemini 4 Neon (High)' },
} satisfies Record<BuiltinProvider, { app: string, model: string }>

interface SetupInput {
  provider: BuiltinProvider
  apiKey: string
  root: string
  windows: boolean
  websocketEnabled: boolean
}

function buildCodexConfig(apiKey: string, baseUrl: string, websocketEnabled: boolean) {
  const model = JSON.stringify(clientPresets.openai.model)
  return `model_provider = "OpenAI"
model = ${model}
review_model = ${model}
model_reasoning_effort = "max"
service_tier = "default"
# 语音通话控制地址
experimental_realtime_ws_base_url = ${JSON.stringify(baseUrl)}

[model_providers.OpenAI]
name = "OpenAI"
base_url = ${JSON.stringify(baseUrl)}
model_catalog_url = ${JSON.stringify(`${baseUrl}/models`)}
wire_api = "responses"
supports_websockets = ${websocketEnabled}
requires_openai_auth = false
# 代理密钥
experimental_bearer_token = ${JSON.stringify(apiKey)}

[model_providers.OpenAI.capabilities]
# V2 远程压缩
remote_compaction = "v2"

[model_providers.OpenAI.http_headers]
# 服务端托管认证
X-OpenAI-Actor-Authorization = "proxy-managed"

[features]
api_key_model_discovery = true
async_compaction = true
image_generation = true
goals = true`
}

export function buildClientConfig({ provider, root, apiKey, windows, websocketEnabled }: SetupInput) {
  const path = (relative: string) => windows
    ? `%USERPROFILE%\\${relative.replaceAll('/', '\\')}`
    : `~/${relative}`
  const baseUrl = `${root}/v1`
  if (provider === 'openai') {
    return {
      title: path('.codex/config.toml'),
      text: buildCodexConfig(apiKey, baseUrl, websocketEnabled),
    }
  }
  if (provider === 'xai') {
    return {
      title: path('.grok/config.toml'),
      text: `[models]
default = "cpr"

[model.cpr]
model = ${JSON.stringify(clientPresets.xai.model)}
base_url = ${JSON.stringify(baseUrl)}
api_key = ${JSON.stringify(apiKey)}
api_backend = "responses"`,
    }
  }

  return {
    title: path('.gemini/config/config.json'),
    text: JSON.stringify({
      gateway: {
        enabled: true,
        url: root,
        wireProtocol: 'genai',
        apiKey,
      },
    }, null, 2),
  }
}

interface CcSwitchImportInput {
  provider: BuiltinProvider
  apiKey: string
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

export function buildCcSwitchImportDeeplink(input: CcSwitchImportInput): string {
  const root = resolveServiceRootUrl()
  const baseUrl = `${root}/v1`
  const endpoint = input.provider === 'antigravity'
    ? root
    : baseUrl
  // 导入端会重建 Codex 配置，只传连接信息和用量查询，不传无法保留的完整模板
  const entries: [string, string][] = [
    ['resource', 'provider'],
    ['app', clientPresets[input.provider].app],
    ['model', clientPresets[input.provider].model],
    ['name', input.providerName],
    ['homepage', root],
    ['endpoint', endpoint],
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
