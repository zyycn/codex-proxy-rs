import type {
  PluginArtifact,
  PluginArtifactMetadata,
  PluginInstance,
  PluginObserverEvent,
  PluginSource,
  PluginUpdateSource,
  VerifyRemotePluginRequest,
} from '@/api'
import { PLUGIN_CAPABILITY_LABELS, PLUGIN_OBSERVER_EVENT_LABELS, PLUGIN_REQUEST_STAGES } from '../constants'

export type PluginInstallMode = 'upload' | 'url' | 'github'
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

export function pluginRequestBindingEntries(metadata: PluginArtifactMetadata) {
  return Object.entries(metadata.contributes).flatMap(([capability, contribution]) =>
    contribution.stages.flatMap((stage) => {
      const description = PLUGIN_REQUEST_STAGES[stage]
      if (!description)
        return []
      const events: { event?: PluginObserverEvent, label: string }[] = capability === 'observer'
        ? Object.entries(PLUGIN_OBSERVER_EVENT_LABELS).map(([event, label]) => ({ event: event as PluginObserverEvent, label }))
        : [{ label: description.label }]
      return events.map(({ event, label }) => ({ ...description, capability, contribution: contribution.id, stage, event, label }))
    }),
  )
}

export function pluginCapabilityLabel(capability: string) {
  return PLUGIN_CAPABILITY_LABELS[capability] ?? capability
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

export function pluginCapabilityForContribution(
  metadata: PluginArtifactMetadata,
  contributionId: string,
) {
  return Object.entries(metadata.contributes).find(([, contribution]) =>
    contribution.id === contributionId,
  )?.[0]
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
