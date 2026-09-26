import type { Ref } from 'vue'
import type { InstalledPlugin } from '../utils/catalog'
import type { PluginArtifact, PluginInstance, PluginRelease, PluginSourceCredential, PluginUpdateSourceBinding, VerifiedPluginArtifact, VerifyRemotePluginRequest } from '@/api'
import { isEqual } from 'es-toolkit'
import { onScopeDispose, shallowRef, watch } from 'vue'
import { checkPluginUpdate, getPluginUpdateSources, queryPluginRelease, verifyRemotePlugin } from '@/api'
import { currentPluginInstance } from '../utils/catalog'
import { selectPluginReleaseAsset } from '../utils/updates'

export interface PluginUpdateSelection {
  binding: PluginUpdateSourceBinding
  credentialIds: string[]
  release?: PluginRelease
  artifact?: VerifiedPluginArtifact
  request?: VerifyRemotePluginRequest
  instance?: PluginInstance
}

export function usePluginUpdateCheck(credentials: Ref<PluginSourceCredential[]>, notifyError: (title: string, error: unknown) => void) {
  const open = shallowRef(false)
  const checking = shallowRef(false)
  const plugin = shallowRef<InstalledPlugin | null>(null)
  const result = shallowRef<PluginUpdateSelection | null>(null)
  let controller: AbortController | undefined

  function cancel() {
    controller?.abort()
    controller = undefined
    checking.value = false
  }

  async function check(target: InstalledPlugin) {
    cancel()
    const current = new AbortController()
    controller = current
    plugin.value = target
    result.value = null
    open.value = true
    checking.value = true
    try {
      const options = { signal: current.signal, silent: true }
      const binding = (await getPluginUpdateSources(options)).find(value => value.pluginId === target.id)
      if (!binding || (binding.source.kind !== 'github' && binding.source.kind !== 'url'))
        throw new Error('此来源不支持检查更新，请安装新版本')
      const source = binding.source
      const history = [...target.artifacts].sort((left, right) => right.installedAt.localeCompare(left.installedAt))
      const previous = history.find((artifact: PluginArtifact) => source.kind === 'github'
        ? artifact.source.kind === 'github' && artifact.source.repository === source.repository
        : artifact.source.kind === 'url' && artifact.source.url === source.url)?.source
      const credentialIds = previous && (previous.kind === 'github' || previous.kind === 'url')
        ? previous.credential_ids.filter(id => credentials.value.some(credential => credential.id === id))
        : []
      let candidate: PluginUpdateSelection
      if (source.kind === 'github') {
        // 手动来源仍可按需查询稳定发布，不为一次查询修改持久化策略。
        const release = binding.policy.kind === 'manual'
          ? await queryPluginRelease({ query: { repository: source.repository, tag: null, allowPrerelease: false }, credentialIds, outboundProxyId: binding.outboundProxyId }, options)
          : (await checkPluginUpdate({ pluginId: target.id, credentialIds }, options)).release
        candidate = { binding, credentialIds, release }
        const asset = selectPluginReleaseAsset(release, target.artifact)
        if (asset) {
          candidate.request = {
            expectedPluginId: target.id,
            credentialIds,
            outboundProxyId: binding.outboundProxyId,
            location: { kind: 'github', repository: release.repository, tag: release.tag, asset: asset.name, allow_prerelease: release.prerelease, sha256: asset.sha256 },
          }
          candidate.artifact = await verifyRemotePlugin(candidate.request, options)
        }
      }
      else {
        const request: VerifyRemotePluginRequest = { expectedPluginId: target.id, credentialIds, outboundProxyId: binding.outboundProxyId, location: { kind: 'url', url: source.url, sha256: null } }
        const artifact = await verifyRemotePlugin(request, options)
        candidate = { binding, credentialIds, artifact, request }
      }
      candidate.instance = currentPluginInstance(target)
      const latest = (await getPluginUpdateSources(options)).find(value => value.pluginId === target.id)
      if (!isEqual(binding, latest))
        throw new Error('安装来源已变更，请重新检查')
      if (controller === current)
        result.value = candidate
    }
    catch (error) {
      if (controller === current) {
        open.value = false
        notifyError('检查更新失败', error)
      }
    }
    finally {
      if (controller === current) {
        controller = undefined
        checking.value = false
      }
    }
  }

  watch(open, (value) => {
    if (!value)
      cancel()
  })
  onScopeDispose(cancel)
  return { open, checking, plugin, result, check }
}
