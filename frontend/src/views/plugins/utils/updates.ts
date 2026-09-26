import type { PluginArtifact, PluginArtifactMetadata, PluginRelease } from '@/api'
import gt from 'semver/functions/gt'
import valid from 'semver/functions/valid'

export function isNewerPluginVersion(current: PluginArtifactMetadata, candidate: PluginArtifactMetadata) {
  return current.pluginId === candidate.pluginId
    && current.sha256 !== candidate.sha256
    && Boolean(valid(current.version) && valid(candidate.version))
    && gt(candidate.version, current.version)
}

export function selectPluginReleaseAsset(release: PluginRelease, previous?: PluginArtifact) {
  const assets = release.assets.filter(asset => /\.(?:tar\.gz|tgz)$/i.test(asset.name))
  if (assets.length === 1)
    return assets[0]
  if (previous?.source.kind !== 'github' || previous.source.repository !== release.repository)
    return undefined
  // 仅沿用已验证包的命名与平台后缀；无法唯一匹配时交由用户选择，最终仍校验包内平台。
  const { asset } = previous.source
  const version = previous.metadata.version
  const index = asset.indexOf(version)
  if (index < 0 || index !== asset.lastIndexOf(version))
    return undefined
  const prefix = asset.slice(0, index)
  const suffix = asset.slice(index + version.length)
  const matches = assets.filter(candidate => candidate.name.startsWith(prefix) && candidate.name.endsWith(suffix))
  return matches.length === 1 ? matches[0] : undefined
}
