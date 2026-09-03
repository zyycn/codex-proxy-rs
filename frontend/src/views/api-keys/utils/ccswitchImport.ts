import {
  buildCodexConfigFiles,
  CODEX_DEFAULT_MODEL,
  CODEX_WEBSOCKET_ENABLED_BY_DEFAULT,
} from './codexConfig'

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
  // Carry the complete files shown in the UI; the explicit fields below keep
  // compatibility with CCSwitch versions that only extract common settings.
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
