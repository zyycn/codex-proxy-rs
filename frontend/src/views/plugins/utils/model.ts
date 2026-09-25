import type {
  PluginArtifact,
  PluginArtifactMetadata,
  PluginInstance,
  PluginInstanceRuntimeStatus,
  PluginSource,
  PluginUpdateSource,
  VerifyRemotePluginRequest,
} from '@/api'

export type PluginInstallSelection = File | VerifyRemotePluginRequest

export function pluginInstallSelectionKey(selection: PluginInstallSelection) {
  return selection instanceof File ? selection : JSON.stringify(selection)
}

export interface JsonSchema {
  type?: string | string[]
  title?: string
  description?: string
  default?: unknown
  enum?: unknown[]
  properties?: Record<string, JsonSchema>
  required?: string[]
  minimum?: number
  maximum?: number
  minLength?: number
  maxLength?: number
  pattern?: string
  items?: JsonSchema
  additionalProperties?: boolean | JsonSchema
}

export const PLUGIN_CAPABILITY_LABELS: Record<string, string> = {
  models: '模型目录',
  authentication: '账号认证',
  frontend_authentication: '客户端认证',
  scheduler: '请求调度',
  model_router: '模型路由',
  model_catalog: '模型目录',
  retry_policy: '重试策略',
  executor: 'Provider 执行',
  middleware: '请求中间件',
  request_lifecycle: '请求生命周期',
  web_socket_observer: 'WebSocket 观察',
  usage: '用量处理',
  command_line: '命令行',
  management: '管理扩展',
  quota: '额度查询',
  request_profile: '请求画像',
  account_management: '账号管理',
  billing: '计费处理',
  maintenance: '维护任务',
}

export function pluginCapabilityLabel(capability: string) {
  return PLUGIN_CAPABILITY_LABELS[capability] ?? capability
}

export function pluginContributionEntries(metadata: PluginArtifactMetadata) {
  return Object.entries(metadata.contributes)
}

export function normalizePluginRepository(value: string) {
  // GitHub 下载端使用小写仓库路径，凭据的路径范围必须采用相同规范。
  return value.trim().replace(/^https:\/\/github\.com\//i, '').replace(/\/$/, '').replace(/\.git$/i, '').toLowerCase()
}

export function formatPluginFileSize(bytes: number) {
  if (bytes < 1024)
    return `${bytes} B`
  if (bytes < 1024 * 1024)
    return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
}

export function pluginContributionForCapability(
  metadata: PluginArtifactMetadata,
  capability: string,
) {
  return metadata.contributes[capability]
}

export function pluginCapabilityForContribution(
  metadata: PluginArtifactMetadata,
  contributionId: string,
) {
  return pluginContributionEntries(metadata).find(([, contribution]) =>
    contribution.id === contributionId,
  )?.[0]
}

export const PLUGIN_RUNTIME_STATUS_LABELS: Record<PluginInstanceRuntimeStatus, string> = {
  disabled: '已停用',
  awaiting_publication: '等待发布',
  preparing: '准备中',
  running: '已启用',
  blocked: '发布阻塞',
  preparation_failed: '准备失败',
  faulted: '运行故障',
  draining: '排空中',
}

export function pluginRuntimeStatusLabel(status: PluginInstanceRuntimeStatus) {
  return PLUGIN_RUNTIME_STATUS_LABELS[status]
}

export function sourceLabel(source: PluginSource | PluginUpdateSource) {
  switch (source.kind) {
    case 'builtin':
      return '随版本提供'
    case 'upload':
      return '本地上传'
    case 'url':
      return '固定 URL'
    case 'github':
      return 'GitHub Release'
  }
}

export function sourceDetail(source: PluginSource | PluginUpdateSource) {
  switch (source.kind) {
    case 'builtin':
      return 'release' in source ? source.release : '发行清单'
    case 'upload':
      return '无远程更新来源'
    case 'url':
      return source.url
    case 'github':
      return 'tag' in source ? `${source.repository}@${source.tag}` : source.repository
  }
}

export function shortDigest(digest: string) {
  return digest.length > 15 ? `${digest.slice(0, 8)}…${digest.slice(-6)}` : digest
}

export function artifactForInstance(instance: PluginInstance, artifacts: PluginArtifact[]) {
  return artifacts.find(artifact => artifact.metadata.sha256 === instance.artifactSha256)
}

export function cloneJsonValue<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

export function configurationForSchema(
  schemaValue: Record<string, unknown>,
  current: Record<string, unknown> = {},
  excludedFields: string[] = [],
) {
  const schema = schemaValue as JsonSchema
  let result: Record<string, unknown> = cloneJsonValue(current)
  for (const [name, property] of Object.entries(schema.properties ?? {})) {
    if (!excludedFields.includes(name) && !Object.hasOwn(result, name) && property.default !== undefined) {
      // 计算属性保留 __proto__ 等合法 JSON 字段，不触发普通对象的原型 setter
      result = { ...result, [name]: cloneJsonValue(property.default) }
    }
  }
  return result
}

export function pluginIdOptions(artifacts: PluginArtifact[]) {
  const names = new Map<string, string>()
  for (const artifact of artifacts)
    names.set(artifact.metadata.pluginId, artifact.metadata.displayName)
  return [...names].map(([value, name]) => ({ value, label: `${name} · ${value}` }))
}

export function uniquePluginArtifacts(artifacts: PluginArtifact[]) {
  return [...artifacts].sort((left, right) => {
    const byId = left.metadata.pluginId.localeCompare(right.metadata.pluginId)
    return byId || right.metadata.version.localeCompare(left.metadata.version, undefined, { numeric: true })
  })
}
