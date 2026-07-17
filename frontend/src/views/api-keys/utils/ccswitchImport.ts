const CODEX_CC_SWITCH_MODEL = 'gpt-5.5'

export interface CodexCcSwitchImportInput {
  apiKey: string
  baseUrl: string
  providerName: string
}

export function buildCodexCcSwitchImportDeeplink(input: CodexCcSwitchImportInput): string {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  const entries: [string, string][] = [
    ['resource', 'provider'],
    ['app', 'codex'],
    ['model', CODEX_CC_SWITCH_MODEL],
    ['name', input.providerName],
    ['homepage', baseUrl],
    ['endpoint', baseUrl],
    ['apiKey', input.apiKey],
    ['configFormat', 'json'],
    ['usageEnabled', 'false'],
  ]

  return `ccswitch://v1/import?${new URLSearchParams(entries).toString()}`
}
