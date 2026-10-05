import type { Ref } from 'vue'
import type { PluginRefreshContext } from '../utils/actions'
import type { ConfigurePluginInstanceRequest, PluginArtifact, PluginInstance, PluginVersionPlan } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { deletePluginInstance, disablePluginInstance, getPluginArtifacts, getPluginInstances, updatePluginInstance } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notifyPluginError } from '../utils/actions'
import { configurationStatus } from '../utils/catalog'

interface InstanceContext extends PluginRefreshContext {
  artifacts: Ref<PluginArtifact[]>
  onEdit: (pluginId: string) => void
  onSaved: () => void
}

export function usePluginInstances({ artifacts, refresh, onEdit, onSaved }: InstanceContext) {
  const configurationArtifact = shallowRef<PluginArtifact | null>(null)
  const showInstance = shallowRef(false)
  const configurationDraft = shallowRef<PluginVersionPlan | null>(null)
  const configurationError = shallowRef('')
  const editingInstance = shallowRef<PluginInstance | null>(null)
  const action = useAsyncAction({
    errorText: false,
    onError: error => notifyPluginError('插件配置保存失败', error),
  })
  const savingInstance = action.loading
  const pendingEnable = shallowRef<{
    request: ConfigurePluginInstanceRequest
    instanceId?: string
    artifact: PluginArtifact
    replacements: (Pick<PluginInstance, 'id' | 'name' | 'revision'> & { version?: string })[]
  } | null>(null)
  const showEnableConfirmation = shallowRef(false)
  const showInstanceDelete = shallowRef(false)
  const pendingDeleteInstance = shallowRef<PluginInstance | null>(null)
  const busyInstanceId = shallowRef('')

  function openEditInstance(instance: PluginInstance) {
    configurationDraft.value = null
    configurationError.value = ''
    configurationArtifact.value = artifacts.value.find(value => value.metadata.sha256 === instance.artifactSha256) ?? null
    onEdit(configurationArtifact.value?.metadata.pluginId ?? '')
    editingInstance.value = instance
    showInstance.value = true
  }

  async function saveInstance(request: ConfigurePluginInstanceRequest) {
    if (savingInstance.value || showEnableConfirmation.value)
      return
    const instance = editingInstance.value
    if (!instance)
      return
    const input = { ...request, expectedRevision: configurationDraft.value?.instanceRevision ?? instance.revision }
    const result = await action.run(async () => {
      if (request.enabled) {
        const pending = await prepareInstanceEnable(input, instance.id)
        if (pending.replacements.length) {
          pendingEnable.value = pending
          showEnableConfirmation.value = true
          return true
        }
      }
      await persistInstance(input, instance.id)
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
    if (instance.loadError) {
      toast.error(instance.loadError)
      return
    }
    if ((instance.enabled && configurationStatus(instance) !== 'failed') || savingInstance.value || showEnableConfirmation.value)
      return
    if (instance.configurationRequired) {
      openEditInstance(instance)
      return
    }
    const result = await action.run(async () => {
      // 不传 secrets，沿用已保存密钥，不读取或回填明文。
      const pending = await prepareInstanceEnable({
        expectedRevision: instance.revision,
        name: instance.name,
        artifactSha256: instance.artifactSha256,
        enabled: true,
        configuration: instance.configuration,
        bindings: instance.bindings,
      }, instance.id)
      if (pending.replacements.length) {
        pendingEnable.value = pending
        showEnableConfirmation.value = true
      }
      else {
        await persistInstance(pending.request, instance.id)
      }
      return true
    }, { onError: error => notifyPluginError('启用配置加载失败', error) })
    if (!result)
      await refresh(true, true)
  }

  async function confirmInstanceEnable() {
    const pending = pendingEnable.value
    if (!showEnableConfirmation.value || !pending || savingInstance.value)
      return
    const result = await action.run(async () => {
      await persistInstance({
        ...pending.request,
        replaceInstances: pending.replacements.map(instance => ({ id: instance.id, expectedRevision: instance.revision })),
      }, pending.instanceId)
      return true
    }, { onError: error => notifyPluginError('配置启用失败', error) })
    // 失败后保留编辑草稿或已保存配置，再次提交需重新读取并确认停用范围。
    showEnableConfirmation.value = false
    if (!result)
      await refresh(true, true)
  }

  async function persistInstance(request: ConfigurePluginInstanceRequest, instanceId?: string) {
    if (!instanceId)
      return
    await updatePluginInstance({ id: instanceId, instance: request }, { silent: true })
    showEnableConfirmation.value = false
    showInstance.value = false
    toast.success(request.replaceInstances?.length ? '已切换当前配置' : request.enabled ? '插件设置已应用' : '设置已保存，插件保持停用')
    await refresh(true)
    onSaved()
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
      notifyPluginError('插件停用失败', error)
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
      toast.success('配置、密钥与私有数据已删除，无法恢复')
      await refresh(true)
    }
    catch (error) {
      notifyPluginError('插件实例删除失败', error)
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  function editVersionPlan(instance: PluginInstance, artifact: PluginArtifact, plan: PluginVersionPlan, error: string) {
    configurationArtifact.value = artifact
    configurationDraft.value = plan
    configurationError.value = error
    editingInstance.value = instance
    showInstance.value = true
  }

  return { configurationArtifact, configurationDraft, configurationError, showInstance, editingInstance, savingInstance, pendingEnable, showEnableConfirmation, busyInstanceId, showInstanceDelete, pendingDeleteInstance, openEditInstance, saveInstance, requestInstanceEnable, confirmInstanceEnable, requestInstanceDisable, requestInstanceDelete, confirmInstanceDelete, editVersionPlan }
}
