import type { PluginUpdateSelection } from './usePluginUpdateCheck'
import type {
  PluginArtifact,
  PluginArtifactMutationResponse,
  PluginInstance,
  PluginSourceCredential,
  PluginUpdateSourceBinding,
} from '@/api'

import { storeToRefs } from 'pinia'
import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import {
  getPluginArtifacts,
  getPluginInstances,
  getPluginSourceCredentials,
  getPluginUpdateSources,
} from '@/api'
import { usePluginViewsStore } from '@/stores/modules/plugin-views'
import { notifyPluginError } from '../utils/actions'
import { currentPluginInstance, groupInstalledPlugins } from '../utils/catalog'
import { usePluginArtifactActions } from './usePluginArtifactActions'
import { usePluginCredentials } from './usePluginCredentials'
import { usePluginInstallation } from './usePluginInstallation'
import { usePluginInstances } from './usePluginInstances'
import { usePluginUninstall } from './usePluginUninstall'
import { usePluginUpdateCheck } from './usePluginUpdateCheck'
import { usePluginVersions } from './usePluginVersions'

export function usePluginManagement() {
  const artifacts = shallowRef<PluginArtifact[]>([])
  const instances = shallowRef<PluginInstance[]>([])
  const sources = shallowRef<PluginUpdateSourceBinding[]>([])
  const credentials = shallowRef<PluginSourceCredential[]>([])
  const extensionDirectory = usePluginViewsStore()
  const { views: extensions } = storeToRefs(extensionDirectory)
  const loading = shallowRef(false)
  const catalog = computed(() => groupInstalledPlugins(artifacts.value, instances.value, sources.value))
  const selectedPluginId = shallowRef('')
  const currentPlugin = computed(() => catalog.value.find(plugin => plugin.id === selectedPluginId.value) ?? null)
  const selectedPlugin = shallowRef<ReturnType<typeof groupInstalledPlugins>[number] | null>(null)
  const showDetail = shallowRef(false)
  const detailSection = shallowRef<'configurations' | 'versions'>('configurations')

  let refreshController: AbortController | undefined

  async function refresh(silent = false, suppressErrors = false) {
    refreshController?.abort()
    const controller = new AbortController()
    refreshController = controller
    if (!silent)
      loading.value = true
    try {
      const options = { signal: controller.signal, silent: true }
      const extensionRequest = extensionDirectory.refresh(true)
        .then(items => ({ ok: true as const, items }))
        .catch((error: unknown) => ({ ok: false as const, error }))
      const [artifactItems, instanceItems, sourceItems, credentialItems, extensionResult] = await Promise.all([
        getPluginArtifacts(options),
        getPluginInstances(options),
        getPluginUpdateSources(options),
        getPluginSourceCredentials(options),
        extensionRequest,
      ])
      if (refreshController !== controller)
        return
      artifacts.value = artifactItems
      instances.value = instanceItems
      sources.value = sourceItems
      credentials.value = credentialItems
      if (!extensionResult.ok && !suppressErrors)
        notifyPluginError('插件扩展页加载失败', extensionResult.error)
    }
    catch (error) {
      if (refreshController === controller && !suppressErrors)
        notifyPluginError('插件数据加载失败', error)
    }
    finally {
      if (refreshController === controller) {
        refreshController = undefined
        loading.value = false
      }
    }
  }

  const artifactActions = usePluginArtifactActions({ refresh })
  const credentialActions = usePluginCredentials({ refresh, credentials })
  function showConfigurations() {
    detailSection.value = 'configurations'
    showDetail.value = true
  }
  const instanceActions = usePluginInstances({
    refresh,
    artifacts,
    onEdit: pluginId => selectedPluginId.value = pluginId,
    onSaved: showConfigurations,
  })
  const versionActions = usePluginVersions({
    refresh,
    catalog,
    instances,
    busyInstanceId: instanceActions.busyInstanceId,
    onApplied: showConfigurations,
    onConfigurationRequired: (...args) => {
      instanceActions.editVersionPlan(...args)
      showDetail.value = false
    },
  })

  const installation = usePluginInstallation({
    onInstalled,
    onSourceSaved: (source) => {
      sources.value = [...sources.value.filter(value => value.pluginId !== source.pluginId), source]
    },
  })
  const updateCheck = usePluginUpdateCheck(credentials)

  async function upgradeCheckedPlugin(selection: PluginUpdateSelection) {
    if (await installation.installUpdate(selection))
      updateCheck.open.value = false
  }

  async function onInstalled({ artifact, defaultInstanceId, configurationRequired }: PluginArtifactMutationResponse, switchTarget?: PluginInstance) {
    const artifactIndex = artifacts.value.findIndex(value => value.metadata.sha256 === artifact.metadata.sha256)
    artifacts.value = artifactIndex < 0
      ? [...artifacts.value, artifact]
      : artifacts.value.map((value, index) => index === artifactIndex ? artifact : value)
    selectedPluginId.value = artifact.metadata.pluginId
    await refresh(true)
    if (switchTarget) {
      await versionActions.applyVersionSwitch(switchTarget, artifact)
      return
    }
    if (configurationRequired && defaultInstanceId) {
      const instance = instances.value.find(instance => instance.id === defaultInstanceId)
      if (instance) {
        instanceActions.openEditInstance(instance)
        return
      }
    }
    const plugin = catalog.value.find(plugin => plugin.id === artifact.metadata.pluginId)
    const current = plugin && currentPluginInstance(plugin)
    if (current && current.artifactSha256 !== artifact.metadata.sha256) {
      versionActions.requestVersionSwitch(artifact)
      return
    }
    detailSection.value = 'configurations'
    showDetail.value = true
  }

  function openDetail(pluginId: string) {
    selectedPluginId.value = pluginId
    const plugin = catalog.value.find(value => value.id === pluginId)
    detailSection.value = plugin?.artifacts.some(artifact => artifact.acceptedAt) ? 'configurations' : 'versions'
    showDetail.value = true
  }

  onMounted(() => void refresh())
  const uninstall = usePluginUninstall(() => refresh(true))
  let pollTimer: ReturnType<typeof setTimeout> | undefined
  let disposed = false
  function schedulePoll() {
    clearTimeout(pollTimer)
    if (disposed)
      return
    if (instances.value.some(instance => instance.enabled && ['awaiting_publication', 'preparing', 'draining'].includes(instance.runtime.status))) {
      pollTimer = setTimeout(async () => {
        await refresh(true, true)
        schedulePoll()
      }, 3000)
    }
  }
  watch(instances, schedulePoll)
  watch([currentPlugin, showDetail], ([plugin, visible]) => {
    if (!visible)
      return
    // 卸载或刷新使条目消失时，保留最后一帧详情直到窗口退场完成。
    if (plugin)
      selectedPlugin.value = plugin
    else
      showDetail.value = false
  })
  onScopeDispose(() => {
    disposed = true
    clearTimeout(pollTimer)
    refreshController?.abort()
  })

  return {
    ...installation,
    ...artifactActions,
    ...credentialActions,
    ...instanceActions,
    ...versionActions,
    updateCheck,
    upgradeCheckedPlugin,
    catalog,
    selectedPlugin,
    showDetail,
    detailSection,
    openDetail,
    uninstall,
    artifacts,
    instances,
    sources,
    credentials,
    extensions,
    loading,
    refresh,
  }
}
