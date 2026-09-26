import type { Ref } from 'vue'
import type { PluginInstallMode } from '../components/PluginInstallModal.vue'
import type { InstalledPlugin } from '../utils/catalog'
import type { PluginInstallSelection } from '../utils/model'
import type { PluginUpdateSelection } from './usePluginUpdateCheck'
import type { PluginArtifact, PluginArtifactMutationResponse, PluginInstance, PluginRelease, PluginUpdateSourceBinding, QueryPluginReleaseRequest, VerifiedPluginArtifact } from '@/api'
import { toast } from '@codex-proxy/ui'
import { isEqual } from 'es-toolkit'
import { onScopeDispose, shallowRef, watch } from 'vue'
import { acceptPluginArtifact, getPluginUpdateSources, installRemotePlugin, queryPluginRelease, updatePluginSource, uploadPluginArtifact, verifyRemotePlugin, verifyUploadedPlugin } from '@/api'
import { pluginInstallSelectionKey } from '../utils/model'

interface InstallationContext {
  runAction: <T>(flag: Ref<boolean>, title: string, task: () => Promise<T>) => Promise<T | undefined>
  notifyError: (title: string, error: unknown) => void
  onInstalled: (result: PluginArtifactMutationResponse, switchTarget?: PluginInstance) => Promise<void>
  onSourceSaved: (source: PluginUpdateSourceBinding) => void
}

export function usePluginInstallation({ runAction, notifyError, onInstalled, onSourceSaved }: InstallationContext) {
  const showInstall = shallowRef(false)
  const installMode = shallowRef<PluginInstallMode>('upload')
  const updateSource = shallowRef<PluginUpdateSourceBinding | null>(null)
  const updateSelection = shallowRef<PluginUpdateSelection | null>(null)
  const acceptanceArtifact = shallowRef<PluginArtifact | null>(null)
  const release = shallowRef<PluginRelease | null>(null)
  const installing = shallowRef(false)
  const queryingRelease = shallowRef(false)
  const verifyingArtifact = shallowRef(false)
  const verifiedArtifact = shallowRef<{ requestKey: string | File, artifact: VerifiedPluginArtifact } | null>(null)

  let releaseController: AbortController | undefined
  let verificationController: AbortController | undefined

  function openInstall(mode: PluginInstallMode) {
    resetArtifactVerification()
    resetReleaseQuery()
    installMode.value = mode
    updateSource.value = null
    updateSelection.value = null
    acceptanceArtifact.value = null
    release.value = null
    showInstall.value = true
  }

  function openAcceptance(artifact: PluginArtifact) {
    resetArtifactVerification()
    resetReleaseQuery()
    updateSource.value = null
    updateSelection.value = null
    acceptanceArtifact.value = artifact
    showInstall.value = true
  }

  function openVersionInstall(plugin: InstalledPlugin) {
    const source = plugin.source
    if (!source || source.source.kind === 'builtin') {
      toast.warning('此插件没有可用的安装来源')
      return
    }
    openInstall(source.source.kind)
    updateSource.value = source
  }

  function changeInstallMode(mode: PluginInstallMode) {
    resetArtifactVerification()
    resetReleaseQuery()
    installMode.value = mode
  }

  function openCheckedUpdate(selection: PluginUpdateSelection) {
    const kind = selection.binding.source.kind
    if (kind !== 'github' && kind !== 'url')
      return
    openInstall(kind)
    updateSource.value = selection.binding
    updateSelection.value = selection
  }

  async function saveInstallSource(source: PluginUpdateSourceBinding) {
    const previous = updateSource.value
    if (!previous || previous.pluginId !== source.pluginId)
      return false
    const result = await runAction(installing, '安装来源保存失败', async () => {
      const current = (await getPluginUpdateSources({ silent: true })).find(value => value.pluginId === source.pluginId)
      if (!isEqual(current, previous))
        throw new Error('安装来源已变更，请关闭后重新打开')
      await updatePluginSource(source, { silent: true })
      updateSource.value = source
      onSourceSaved(source)
      return true
    })
    return result === true
  }

  function resetReleaseQuery() {
    releaseController?.abort()
    releaseController = undefined
    queryingRelease.value = false
    release.value = null
  }

  async function queryRelease(request: QueryPluginReleaseRequest) {
    releaseController?.abort()
    const controller = new AbortController()
    releaseController = controller
    queryingRelease.value = true
    release.value = null
    try {
      const result = await queryPluginRelease(request, { signal: controller.signal, silent: true })
      if (releaseController === controller)
        release.value = result
    }
    catch (error) {
      if (releaseController === controller)
        notifyError('GitHub Release 查询失败', error)
    }
    finally {
      if (releaseController === controller) {
        releaseController = undefined
        queryingRelease.value = false
      }
    }
  }

  async function installArtifact(request: PluginInstallSelection) {
    const verified = verifiedArtifact.value
    if (!verified || verified.requestKey !== pluginInstallSelectionKey(request)) {
      toast.warning('请先校验当前选择的插件包')
      return
    }
    const result = await runAction(installing, '插件安装失败', async () => {
      const { pluginId, version, sha256 } = verified.artifact.metadata
      if (request instanceof File) {
        return uploadPluginArtifact(request, sha256, { silent: true })
      }
      else {
        return installRemotePlugin({
          pluginId,
          version,
          credentialIds: request.credentialIds,
          outboundProxyId: request.outboundProxyId,
          location: { ...request.location, sha256 },
        }, { silent: true })
      }
    })
    if (!result)
      return
    showInstall.value = false
    toast.success(result.configurationRequired ? '插件已安装，请补充必要配置' : result.defaultInstanceId ? '插件已安装，正在准备' : '插件版本已安装')
    await onInstalled(result)
  }

  async function installUpdate(selection: PluginUpdateSelection) {
    const { artifact, request, instance, binding } = selection
    if (!artifact || !request || !instance)
      return false
    const completed = await runAction(installing, '插件升级失败', async () => {
      const latest = (await getPluginUpdateSources({ silent: true })).find(value => value.pluginId === binding.pluginId)
      if (!isEqual(binding, latest))
        throw new Error('安装来源已变更，请重新检查更新')
      const { pluginId, version, sha256 } = artifact.metadata
      const result = await installRemotePlugin({
        pluginId,
        version,
        credentialIds: request.credentialIds,
        outboundProxyId: request.outboundProxyId,
        location: { ...request.location, sha256 },
      }, { silent: true })
      // 使用检查时的实例 revision，避免下载期间的设置变更被升级覆盖。
      await onInstalled(result, instance)
      return true
    })
    return completed === true
  }

  async function acceptArtifact(artifact: PluginArtifact) {
    if (acceptanceArtifact.value?.metadata.sha256 !== artifact.metadata.sha256) {
      toast.warning('请选择待安装的插件版本')
      return
    }
    const result = await runAction(installing, '插件安装失败', () => acceptPluginArtifact({ sha256: artifact.metadata.sha256 }, { silent: true }))
    if (!result)
      return
    showInstall.value = false
    toast.success(result.configurationRequired ? '插件已安装，请补充必要配置' : result.defaultInstanceId ? '插件已安装，正在准备' : '插件版本已安装')
    await onInstalled(result)
  }

  function resetArtifactVerification() {
    verificationController?.abort()
    verificationController = undefined
    verifiedArtifact.value = null
    verifyingArtifact.value = false
  }

  async function verifyArtifact(request: PluginInstallSelection) {
    resetArtifactVerification()
    const controller = new AbortController()
    verificationController = controller
    verifyingArtifact.value = true
    try {
      const options = { signal: controller.signal, silent: true }
      const artifact = request instanceof File
        ? await verifyUploadedPlugin(request, options)
        : await verifyRemotePlugin(request, options)
      if (updateSource.value && artifact.metadata.pluginId !== updateSource.value.pluginId)
        throw new Error('插件包与当前插件不符，请重新选择')
      if (verificationController === controller)
        verifiedArtifact.value = { requestKey: pluginInstallSelectionKey(request), artifact }
    }
    catch (error) {
      if (verificationController === controller)
        notifyError('插件包校验失败', error)
    }
    finally {
      if (verificationController === controller) {
        verificationController = undefined
        verifyingArtifact.value = false
      }
    }
  }

  watch(showInstall, (open) => {
    if (!open) {
      resetReleaseQuery()
      resetArtifactVerification()
      acceptanceArtifact.value = null
    }
  })
  onScopeDispose(() => {
    releaseController?.abort()
    verificationController?.abort()
  })
  return {
    showInstall,
    installMode,
    updateSource,
    updateSelection,
    openCheckedUpdate,
    acceptanceArtifact,
    openAcceptance,
    openVersionInstall,
    changeInstallMode,
    saveInstallSource,
    release,
    installing,
    queryingRelease,
    verifiedArtifact,
    verifyingArtifact,
    verifyArtifact,
    resetArtifactVerification,
    openInstall,
    resetReleaseQuery,
    queryRelease,
    installArtifact,
    installUpdate,
    acceptArtifact,
  }
}
