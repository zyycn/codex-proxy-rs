import {
  buildCodexConfigFiles,
  CODEX_DEFAULT_MODEL,
  CODEX_WEBSOCKET_ENABLED_BY_DEFAULT,
} from './codexConfig.ts'

export interface CodexCcSwitchImportInput {
  apiKey: string
  baseUrl: string
  providerName: string
}

export function buildCodexCcSwitchImportDeeplink(input: CodexCcSwitchImportInput): string {
  const configFiles = buildCodexConfigFiles({
    apiKey: input.apiKey,
    baseUrl: input.baseUrl,
    websocketEnabled: CODEX_WEBSOCKET_ENABLED_BY_DEFAULT,
  })
  // 与界面共用原生生图配置；保留 auth 载荷和独立字段，兼容旧版 CCSwitch。
  const config = encodeBase64(JSON.stringify({
    auth: configFiles.auth,
    config: configFiles.configToml,
  }))
  const entries: [string, string][] = [
    ['resource', 'provider'],
    ['app', 'codex'],
    ['model', CODEX_DEFAULT_MODEL],
    ['name', input.providerName],
    ['homepage', configFiles.baseUrl],
    ['endpoint', configFiles.baseUrl],
    ['apiKey', input.apiKey],
    ['configFormat', 'json'],
    ['config', config],
    ['usageEnabled', 'false'],
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
