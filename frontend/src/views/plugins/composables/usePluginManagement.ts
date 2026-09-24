import type { Ref } from 'vue'
import type {
  ConfigurePluginInstanceRequest,
  CreatePluginSourceCredentialRequest,
  PluginArtifact,
  PluginArtifactMutationResponse,
  PluginInstance,
  PluginRollbackPlan,
  PluginSourceCredential,
  PluginUpdateSourceBinding,
  PluginVersionPlan,
} from '@/api'

import { toast } from '@codex-proxy/ui'
import { storeToRefs } from 'pinia'
import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import {
  createPluginSourceCredential,
  deletePluginArtifact,
  deletePluginInstance,
  deletePluginSourceCredential,
  disablePluginInstance,
  getPluginArtifacts,
  getPluginInstances,
  getPluginRollbackPlan,
  getPluginSourceCredentials,
  getPluginUpdateSources,
  getPluginVersionPlan,
  rollbackPluginInstance,
  switchPluginVersion,
  updatePluginInstance,
} from '@/api'
import { ApiError } from '@/api/request'
import { usePluginManagementViewsStore } from '@/stores/modules/plugin-management-views'
import { errorMessage } from '@/utils/async'
import { configurationStatus, currentPluginInstance, groupInstalledPlugins } from '../utils/catalog'
import { usePluginInstallation } from './usePluginInstallation'
import { usePluginUninstall } from './usePluginUninstall'
import { usePluginUpdateCheck } from './usePluginUpdateCheck'

export function usePluginManagement() {
  const artifacts = shallowRef<PluginArtifact[]>([])
  const instances = shallowRef<PluginInstance[]>([])
  const sources = shallowRef<PluginUpdateSourceBinding[]>([])
  const credentials = shallowRef<PluginSourceCredential[]>([])
  const extensionDirectory = usePluginManagementViewsStore()
  const { views: extensions } = storeToRefs(extensionDirectory)
  const loading = shallowRef(false)
  const catalog = computed(() => groupInstalledPlugins(artifacts.value, instances.value, sources.value))
  const selectedPluginId = shallowRef('')
  const selectedPlugin = computed(() => catalog.value.find(plugin => plugin.id === selectedPluginId.value) ?? null)
  const showDetail = shallowRef(false)
  const detailSection = shallowRef<'configurations' | 'versions'>('configurations')
  const configurationArtifact = shallowRef<PluginArtifact | null>(null)
  const showInstance = shallowRef(false)
  const configurationDraft = shallowRef<PluginVersionPlan | null>(null)
  const configurationError = shallowRef('')
  const editingInstance = shallowRef<PluginInstance | null>(null)
  const savingInstance = shallowRef(false)
  const pendingEnable = shallowRef<{
    request: ConfigurePluginInstanceRequest
    instanceId?: string
    artifact: PluginArtifact
    replacements: (Pick<PluginInstance, 'id' | 'name' | 'revision'> & { version?: string })[]
  } | null>(null)
  const showEnableConfirmation = computed({
    get: () => pendingEnable.value !== null,
    set: (open: boolean) => {
      if (!open && !savingInstance.value)
        pendingEnable.value = null
    },
  })
  const showRollback = shallowRef(false)
  const rollbackInstance = shallowRef<PluginInstance | null>(null)
  const rollbackPlan = shallowRef<PluginRollbackPlan | null>(null)
  const loadingRollback = shallowRef(false)

  const savingCredential = shallowRef(false)

  const showArtifactDelete = shallowRef(false)
  const pendingArtifact = shallowRef<PluginArtifact | null>(null)
  const showInstanceDelete = shallowRef(false)
  const pendingDeleteInstance = shallowRef<PluginInstance | null>(null)
  const showCredentialDelete = shallowRef(false)
  const pendingCredential = shallowRef<PluginSourceCredential | null>(null)

  const busyDigest = shallowRef('')
  const busyInstanceId = shallowRef('')
  const busyDistributionId = shallowRef('')
  const pendingVersionSwitch = shallowRef<{ instance: PluginInstance, artifact: PluginArtifact, currentVersion: string } | null>(null)
  const showVersionSwitch = computed({
    get: () => pendingVersionSwitch.value !== null,
    set: (open: boolean) => {
      if (!open && !busyInstanceId.value)
        pendingVersionSwitch.value = null
    },
  })
  let refreshController: AbortController | undefined
  let rollbackController: AbortController | undefined

  function notifyError(title: string, error: unknown) {
    if (error instanceof ApiError && error.kind === 'cancelled')
      return
    toast.error(errorMessage(error, title))
  }

  async function refresh(silent = false, suppressErrors = false) {
    refreshController?.abort()
    const controller = new AbortController()
    refreshController = controller
    if (!silent)
      loading.value = true
    try {
      const options = { signal: controller.signal, silent: true }
      const extensionRequest = extensionDirectory.refresh(silent)
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
        notifyError('插件扩展页加载失败', extensionResult.error)
    }
    catch (error) {
      if (refreshController === controller && !suppressErrors)
        notifyError('插件数据加载失败', error)
    }
    finally {
      if (refreshController === controller) {
        refreshController = undefined
        loading.value = false
      }
    }
  }

  async function runAction<T>(
    flag: Ref<boolean>,
    title: string,
    task: () => Promise<T>,
  ): Promise<T | undefined> {
    if (flag.value)
      return undefined
    flag.value = true
    try {
      return await task()
    }
    catch (error) {
      notifyError(title, error)
      return undefined
    }
    finally {
      flag.value = false
    }
  }

  const installation = usePluginInstallation({
    runAction,
    notifyError,
    onInstalled,
    onSourceSaved: (source) => {
      sources.value = [...sources.value.filter(value => value.pluginId !== source.pluginId), source]
    },
  })
  const updateCheck = usePluginUpdateCheck(credentials, notifyError)

  async function onInstalled({ artifact, defaultInstanceId, configurationRequired }: PluginArtifactMutationResponse) {
    const artifactIndex = artifacts.value.findIndex(value => value.metadata.sha256 === artifact.metadata.sha256)
    artifacts.value = artifactIndex < 0
      ? [...artifacts.value, artifact]
      : artifacts.value.map((value, index) => index === artifactIndex ? artifact : value)
    selectedPluginId.value = artifact.metadata.pluginId
    await refresh(true)
    if (configurationRequired && defaultInstanceId) {
      const instance = instances.value.find(instance => instance.id === defaultInstanceId)
      if (instance) {
        openEditInstance(instance)
        return
      }
    }
    const plugin = catalog.value.find(plugin => plugin.id === artifact.metadata.pluginId)
    const current = plugin && currentPluginInstance(plugin)
    if (current && current.artifactSha256 !== artifact.metadata.sha256) {
      requestVersionSwitch(artifact)
      return
    }
    detailSection.value = 'configurations'
    showDetail.value = true
  }

  function requestArtifactDelete(artifact: PluginArtifact) {
    pendingArtifact.value = artifact
    showArtifactDelete.value = true
  }

  async function confirmArtifactDelete() {
    const artifact = pendingArtifact.value
    if (!artifact || busyDigest.value)
      return
    busyDigest.value = artifact.metadata.sha256
    try {
      await deletePluginArtifact({ sha256: artifact.metadata.sha256 }, { silent: true })
      showArtifactDelete.value = false
      pendingArtifact.value = null
      toast.success('插件制品已删除')
      await refresh(true)
    }
    catch (error) {
      notifyError('插件制品删除失败', error)
    }
    finally {
      busyDigest.value = ''
    }
  }

  function openDetail(pluginId: string) {
    selectedPluginId.value = pluginId
    const plugin = catalog.value.find(value => value.id === pluginId)
    detailSection.value = plugin?.artifacts.some(artifact => artifact.acceptedAt) ? 'configurations' : 'versions'
    showDetail.value = true
  }

  function openEditInstance(instance: PluginInstance) {
    configurationDraft.value = null
    configurationError.value = ''
    configurationArtifact.value = artifacts.value.find(value => value.metadata.sha256 === instance.artifactSha256) ?? null
    selectedPluginId.value = configurationArtifact.value?.metadata.pluginId ?? ''
    editingInstance.value = instance
    showInstance.value = true
  }

  async function saveInstance(request: ConfigurePluginInstanceRequest) {
    if (savingInstance.value || pendingEnable.value)
      return
    const instance = editingInstance.value
    if (!instance)
      return
    const input = { ...request, expectedRevision: configurationDraft.value?.instanceRevision ?? instance.revision }
    const result = await runAction(savingInstance, '插件配置保存失败', async () => {
      if (request.enabled) {
        const pending = await prepareInstanceEnable(input, instance?.id)
        if (pending.replacements.length) {
          pendingEnable.value = pending
          return true
        }
      }
      await persistInstance(input, instance?.id)
      return true
    })
    if (!result)
      await refresh(true, true)
  }

  async function prepareInstanceEnable(request: ConfigurePluginInstanceRequest, instanceId?: string) {
    // 启用入口共用确认快照，不自动停用确认后出现或修改的配置。
    const [currentArtifacts, currentInstances] = await Promise.all([
      getPluginArtifacts({ silent: true }),
      getPluginInstances({ silent: true }),
    ])
    const artifact = currentArtifacts.find(artifact => artifact.metadata.sha256 === request.artifactSha256)
    if (!artifact)
      throw new Error('所选版本已不存在，请重新选择')
    const digests = new Set(currentArtifacts.filter(value => value.metadata.pluginId === artifact.metadata.pluginId).map(value => value.metadata.sha256))
    const replacements = currentInstances
      .filter(current => current.enabled && current.id !== (instanceId ?? request.creationId) && digests.has(current.artifactSha256))
      .map(({ id, name, revision, artifactSha256 }) => ({ id, name, revision, version: currentArtifacts.find(value => value.metadata.sha256 === artifactSha256)?.metadata.version }))
    return { request, instanceId, artifact, replacements }
  }

  async function requestInstanceEnable(instance: PluginInstance) {
    if ((instance.enabled && configurationStatus(instance) !== 'failed') || savingInstance.value || pendingEnable.value)
      return
    if (instance.configurationRequired) {
      openEditInstance(instance)
      return
    }
    const result = await runAction(savingInstance, '启用配置加载失败', async () => {
      // 不传 secrets，沿用已保存密钥，不读取或回填明文。
      const pending = await prepareInstanceEnable({
        expectedRevision: instance.revision,
        name: instance.name,
        artifactSha256: instance.artifactSha256,
        enabled: true,
        configuration: instance.configuration,
        bindings: instance.bindings,
      }, instance.id)
      if (pending.replacements.length)
        pendingEnable.value = pending
      else await persistInstance(pending.request, instance.id)
      return true
    })
    if (!result)
      await refresh(true, true)
  }

  async function confirmInstanceEnable() {
    const pending = pendingEnable.value
    if (!pending || savingInstance.value)
      return
    const result = await runAction(savingInstance, '配置启用失败', async () => {
      await persistInstance({
        ...pending.request,
        replaceInstances: pending.replacements.map(instance => ({ id: instance.id, expectedRevision: instance.revision })),
      }, pending.instanceId)
      return true
    })
    // 失败后保留编辑草稿或已保存配置，再次提交需重新读取并确认停用范围。
    pendingEnable.value = null
    if (!result)
      await refresh(true, true)
  }

  async function persistInstance(request: ConfigurePluginInstanceRequest, instanceId?: string) {
    if (!instanceId)
      return
    await updatePluginInstance({ id: instanceId, instance: request }, { silent: true })
    pendingEnable.value = null
    showInstance.value = false
    editingInstance.value = null
    toast.success(request.replaceInstances?.length ? '已切换当前配置' : request.enabled ? '插件设置已应用' : '设置已保存，插件保持停用')
    await refresh(true)
    detailSection.value = 'configurations'
    showDetail.value = true
  }

  async function requestInstanceDisable(instance: PluginInstance) {
    if (busyInstanceId.value)
      return
    busyInstanceId.value = instance.id
    try {
      await disablePluginInstance({ id: instance.id }, { silent: true })
      toast.success('插件已停用')
      await refresh(true)
    }
    catch (error) {
      notifyError('插件停用失败', error)
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  function requestVersionSwitch(artifact: PluginArtifact) {
    const plugin = catalog.value.find(plugin => plugin.id === artifact.metadata.pluginId)
    const instance = plugin && currentPluginInstance(plugin)
    if (!plugin || !instance || !artifact.acceptedAt || busyInstanceId.value || instance.artifactSha256 === artifact.metadata.sha256)
      return
    pendingVersionSwitch.value = { instance, artifact, currentVersion: plugin.artifact.metadata.version }
  }

  async function confirmVersionSwitch() {
    if (!pendingVersionSwitch.value || busyInstanceId.value)
      return
    const { instance, artifact } = pendingVersionSwitch.value
    busyInstanceId.value = instance.id
    try {
      await switchPluginVersion({
        id: instance.id,
        target: { artifactSha256: artifact.metadata.sha256, expectedRevision: instance.revision },
      }, { silent: true })
      pendingVersionSwitch.value = null
      toast.success(`已切换至 ${artifact.metadata.version}`)
      await refresh(true)
      detailSection.value = 'configurations'
      showDetail.value = true
    }
    catch (error) {
      await refresh(true, true)
      const current = instances.value.find(value => value.id === instance.id)
      if (error instanceof ApiError && error.status === 400 && current?.revision === instance.revision) {
        try {
          const plan = await getPluginVersionPlan(instance.id, artifact.metadata.sha256, { silent: true })
          if (plan.instanceRevision !== instance.revision)
            throw new Error('插件设置已变更，请刷新后重试')
          configurationArtifact.value = artifact
          pendingVersionSwitch.value = null
          configurationDraft.value = plan
          configurationError.value = errorMessage(error, '请调整不兼容的设置后重试')
          editingInstance.value = current
          showDetail.value = false
          showInstance.value = true
        }
        catch (planError) {
          notifyError('版本设置加载失败', planError)
        }
      }
      else {
        notifyError('版本切换失败', error)
      }
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  async function openRollback(instance: PluginInstance) {
    rollbackController?.abort()
    const controller = new AbortController()
    rollbackController = controller
    rollbackInstance.value = instance
    rollbackPlan.value = null
    showRollback.value = true
    loadingRollback.value = true
    try {
      const plan = await getPluginRollbackPlan(instance.id, { signal: controller.signal, silent: true })
      if (rollbackController === controller)
        rollbackPlan.value = plan
    }
    catch (error) {
      if (rollbackController === controller)
        notifyError('回退版本加载失败', error)
    }
    finally {
      if (rollbackController === controller) {
        rollbackController = undefined
        loadingRollback.value = false
      }
    }
  }

  async function confirmRollback(artifactSha256: string) {
    const instance = rollbackInstance.value
    const plan = rollbackPlan.value
    const target = plan?.targets.find(target => target.artifactSha256 === artifactSha256)
    if (!instance || !plan || !target || busyInstanceId.value)
      return
    busyInstanceId.value = instance.id
    try {
      await rollbackPluginInstance({ id: instance.id, target: { artifactSha256, expectedRevision: plan.instanceRevision } }, { silent: true })
      showRollback.value = false
      rollbackInstance.value = null
      rollbackPlan.value = null
      toast.success(`已回退至 ${target.version}，并恢复对应设置`)
      await refresh(true)
    }
    catch (error) {
      notifyError('插件版本回退失败', error)
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  function requestInstanceDelete(instance: PluginInstance) {
    pendingDeleteInstance.value = instance
    showInstanceDelete.value = true
  }

  async function confirmInstanceDelete() {
    const instance = pendingDeleteInstance.value
    if (!instance || busyInstanceId.value)
      return
    busyInstanceId.value = instance.id
    try {
      await deletePluginInstance({ id: instance.id }, { silent: true })
      showInstanceDelete.value = false
      pendingDeleteInstance.value = null
      toast.success('配置、密钥与私有数据已删除，无法恢复')
      await refresh(true)
    }
    catch (error) {
      notifyError('插件实例删除失败', error)
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  async function saveCredential(request: CreatePluginSourceCredentialRequest) {
    const result = await runAction(savingCredential, '下载认证保存失败', () => createPluginSourceCredential(request, { silent: true }))
    if (!result)
      return null
    credentials.value = [...credentials.value, result]
    return result
  }

  function requestCredentialDelete(credential: PluginSourceCredential) {
    pendingCredential.value = credential
    showCredentialDelete.value = true
  }

  async function confirmCredentialDelete() {
    const credential = pendingCredential.value
    if (!credential || busyDistributionId.value)
      return
    busyDistributionId.value = credential.id
    try {
      await deletePluginSourceCredential({ id: credential.id }, { silent: true })
      showCredentialDelete.value = false
      pendingCredential.value = null
      toast.success('来源凭据已删除')
      await refresh(true)
    }
    catch (error) {
      notifyError('来源凭据删除失败', error)
    }
    finally {
      busyDistributionId.value = ''
    }
  }

  watch(showInstance, (open) => {
    if (!open && !savingInstance.value) {
      editingInstance.value = null
      pendingEnable.value = null
    }
  })
  watch(showRollback, (open) => {
    if (!open) {
      rollbackController?.abort()
      rollbackController = undefined
      loadingRollback.value = false
    }
  })

  onMounted(() => void refresh())
  const uninstall = usePluginUninstall(() => refresh(true), notifyError)
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
  watch(selectedPlugin, (plugin) => {
    if (!plugin)
      showDetail.value = false
  })
  onScopeDispose(() => {
    disposed = true
    clearTimeout(pollTimer)
    refreshController?.abort()
    rollbackController?.abort()
  })

  return {
    ...installation,
    updateCheck,
    catalog,
    selectedPlugin,
    showDetail,
    detailSection,
    openDetail,
    configurationArtifact,
    configurationDraft,
    configurationError,
    uninstall,
    artifacts,
    instances,
    sources,
    credentials,
    extensions,
    loading,
    showInstance,
    editingInstance,
    savingInstance,
    pendingEnable,
    showEnableConfirmation,
    confirmInstanceEnable,
    showRollback,
    rollbackInstance,
    rollbackPlan,
    loadingRollback,
    openRollback,
    confirmRollback,
    savingCredential,
    requestVersionSwitch,
    confirmVersionSwitch,
    pendingVersionSwitch,
    showVersionSwitch,
    showArtifactDelete,
    pendingArtifact,
    showInstanceDelete,
    pendingDeleteInstance,
    showCredentialDelete,
    pendingCredential,
    busyDigest,
    busyInstanceId,
    busyDistributionId,
    refresh,
    requestArtifactDelete,
    confirmArtifactDelete,
    openEditInstance,
    saveInstance,
    requestInstanceEnable,
    requestInstanceDisable,
    requestInstanceDelete,
    confirmInstanceDelete,
    saveCredential,
    requestCredentialDelete,
    confirmCredentialDelete,
  }
}
