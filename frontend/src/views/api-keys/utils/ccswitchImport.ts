const OPENAI_CC_SWITCH_MODEL = 'gpt-5.5'
const XAI_CC_SWITCH_MODEL = 'grok-4.5'

export interface CodexCcSwitchImportInput {
  apiKey: string
  baseUrl: string
  providerName: string
  providerKind: string
}

export function buildCodexCcSwitchImportDeeplink(input: CodexCcSwitchImportInput): string {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  const model = input.providerKind.trim().toLowerCase() === 'xai'
    ? XAI_CC_SWITCH_MODEL
    : OPENAI_CC_SWITCH_MODEL
  const entries: [string, string][] = [
    ['resource', 'provider'],
    ['app', 'codex'],
    ['model', model],
    ['name', input.providerName],
    ['homepage', baseUrl],
    ['endpoint', baseUrl],
    ['apiKey', input.apiKey],
    ['configFormat', 'json'],
    ['usageEnabled', 'false'],
  ]

  return `ccswitch://v1/import?${new URLSearchParams(entries).toString()}`
}
