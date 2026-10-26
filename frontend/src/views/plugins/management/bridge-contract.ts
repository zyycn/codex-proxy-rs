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

export type PluginPageMessageType = 'request' | 'callback-ticket' | 'resize' | 'models-request' | 'models-pull' | 'models-cancel'
export type PluginModelMessageType = 'models-response' | 'models-chunk' | 'models-complete' | 'models-error'

export interface PluginBridgeMessage {
  target: string
  version: number
  channel: string
  session: string
  type: string
  id: number
  payload?: unknown
}

export interface PluginModelMessage {
  type: PluginModelMessageType
  id: number
  payload?: Record<string, unknown>
  error?: unknown
}

export interface PluginManagementRequest {
  method: string
  path: string
  query?: string
  contentType?: string
  body?: string | ArrayBuffer | ArrayBufferView
}

export interface PluginManagementReply {
  status: number
  contentType: string
  body: ArrayBuffer
}

export interface PluginCallbackTicket {
  state: string
  expiresAtMs: number
  callbackUrl: string
}

export interface PluginModelRequest {
  clientKeyId: string
  body: Record<string, unknown>
  signal?: AbortSignal
}

// 注入 API 的公开类型与实际安装对象共用合同，不暴露内部消息或资源索引
export interface PluginHostBridge {
  readonly version: typeof PLUGIN_MANAGEMENT_BRIDGE_VERSION
  readonly plugin: Readonly<{ name: string }>
  readonly page: Readonly<{ id: string, title: string }>
  readonly theme: 'light' | 'dark'
  request: (input: PluginManagementRequest) => Promise<PluginManagementReply>
  callbackTicket: (input: { path: string, ttlSeconds: number }) => Promise<PluginCallbackTicket>
  resourceUrl: (path: string) => string
  readonly models: Readonly<{ responses: (input: PluginModelRequest) => Promise<Response> }>
}

export interface PluginFrameTheme {
  name: 'light' | 'dark'
  tokens: Record<string, string>
}

export interface PluginBridgeInitial {
  target: string
  version: typeof PLUGIN_MANAGEMENT_BRIDGE_VERSION
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

export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export function isPluginBridgeRequest(value: unknown): value is PluginBridgeMessage {
  return isRecord(value)
    && value.target === PLUGIN_MANAGEMENT_BRIDGE
    && value.version === PLUGIN_MANAGEMENT_BRIDGE_VERSION
    && typeof value.channel === 'string'
    && typeof value.session === 'string'
    && typeof value.type === 'string'
    && Number.isSafeInteger(value.id)
    && Number(value.id) > 0
}

export function hasControlCharacter(value: string) {
  return [...value].some((character) => {
    const code = character.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7F
  })
}
