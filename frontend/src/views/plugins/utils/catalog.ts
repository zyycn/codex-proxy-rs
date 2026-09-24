import type { PluginArtifact, PluginInstance, PluginUpdateSourceBinding } from '@/api'

export interface InstalledPlugin {
  id: string
  artifact: PluginArtifact
  artifacts: PluginArtifact[]
  configurations: PluginInstance[]
  source?: PluginUpdateSourceBinding
}

export type PluginCatalogStatus = 'unaccepted' | 'unconfigured' | 'enabled' | 'disabled' | 'pending' | 'failed'

export function groupInstalledPlugins(artifacts: PluginArtifact[], instances: PluginInstance[], sources: PluginUpdateSourceBinding[]): InstalledPlugin[] {
  const groups = new Map<string, InstalledPlugin>()
  // 尚无当前配置时使用最近安装的包，不把字符串排序误当作语义版本比较。
  for (const artifact of [...artifacts].sort((a, b) => b.installedAt.localeCompare(a.installedAt))) {
    const id = artifact.metadata.pluginId
    const group = groups.get(id)
    if (group)
      group.artifacts.push(artifact)
    else groups.set(id, { id, artifact, artifacts: [artifact], configurations: [], source: sources.find(source => source.pluginId === id) })
  }
  const artifactPlugins = new Map(artifacts.map(artifact => [artifact.metadata.sha256, artifact.metadata.pluginId]))
  for (const instance of instances) {
    const id = artifactPlugins.get(instance.artifactSha256)
    if (id)
      groups.get(id)?.configurations.push(instance)
  }
  for (const plugin of groups.values()) {
    const current = currentPluginInstance(plugin)
    plugin.artifact = plugin.artifacts.find(artifact => artifact.metadata.sha256 === current?.artifactSha256) ?? plugin.artifact
  }
  return [...groups.values()]
}

export function configurationStatus(instance: PluginInstance): PluginCatalogStatus {
  if (instance.configurationRequired)
    return 'unconfigured'
  if (!instance.enabled)
    return 'disabled'
  if (['blocked', 'preparation_failed', 'faulted'].includes(instance.runtime.status))
    return 'failed'
  return instance.runtime.status === 'running' ? 'enabled' : 'pending'
}

export function pluginStatus(plugin: InstalledPlugin): PluginCatalogStatus {
  if (!plugin.artifacts.some(artifact => artifact.acceptedAt))
    return 'unaccepted'
  if (plugin.configurations.filter(instance => instance.enabled).length > 1)
    return 'failed'
  const instance = currentPluginInstance(plugin)
  return instance ? configurationStatus(instance) : 'unconfigured'
}

export const PLUGIN_STATUS_LABELS: Record<PluginCatalogStatus, string> = {
  unaccepted: '待安装',
  unconfigured: '待配置',
  enabled: '已启用',
  disabled: '已停用',
  pending: '等待生效',
  failed: '需要处理',
}

export function pluginStatusType(status: PluginCatalogStatus) {
  if (status === 'enabled')
    return 'success' as const
  if (status === 'failed')
    return 'danger' as const
  if (status === 'pending' || status === 'unconfigured' || status === 'unaccepted')
    return 'warning' as const
  return 'neutral' as const
}

export function currentPluginInstance(plugin: InstalledPlugin): PluginInstance | undefined {
  return [...plugin.configurations].sort((left, right) => Number(right.enabled) - Number(left.enabled) || right.revision - left.revision)[0]
}

export function hasPluginSettings(artifact: PluginArtifact): boolean {
  const schema = artifact.metadata.configurationSchema
  return Boolean(artifact.metadata.secretFields.length
    || Object.keys((schema.properties ?? {}) as Record<string, unknown>).length
    || schema.additionalProperties !== false
    || artifact.metadata.contributes.frontend_authentication)
}
