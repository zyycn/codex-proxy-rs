export const PLUGIN_MANAGEMENT_BRIDGE = 'codex-proxy-plugin-management'
export const PLUGIN_MANAGEMENT_BRIDGE_VERSION = 2
export const MAXIMUM_MANAGEMENT_BODY_BYTES = 1024 * 1024
export const MAXIMUM_MANAGEMENT_QUERY_BYTES = 8192
export const MAXIMUM_MANAGEMENT_IN_FLIGHT = 8
export const MAXIMUM_MODEL_BODY_BYTES = 8 * 1024 * 1024
export const MAXIMUM_MODEL_CHUNK_BYTES = 64 * 1024
export const MAXIMUM_MODEL_IN_FLIGHT = 4
export const MODEL_REQUEST_DEADLINE_MS = 10 * 60 * 1000
export const MODEL_RESPONSE_IDLE_MS = 30 * 1000

export interface PluginFrameTheme {
  name: 'light' | 'dark'
  tokens: Record<string, string>
}

export interface PluginBridgeInitial {
  target: string
  version: number
  channel: string
  session: string
  parentOrigin: string
  plugin: { name: string }
  page: { id: string, title: string }
  theme: PluginFrameTheme
  viewportHeight: number
  resourceUrls: Record<string, string>
  managementTimeoutMs: number
  apiTimeoutMs: number
}

export function hasControlCharacter(value: string) {
  return [...value].some((character) => {
    const code = character.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7F
  })
}
