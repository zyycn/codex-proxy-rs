import type { Ref } from 'vue'
import type { PluginRefreshContext } from '../utils/actions'
import type { InstalledPlugin } from '../utils/catalog'
import type { PluginArtifact, PluginInstance, PluginRollbackPlan, PluginVersionPlan } from '@/api'
import { toast } from '@codex-proxy/ui'
import { onScopeDispose, shallowRef, watch } from 'vue'
import { getPluginRollbackPlan, getPluginVersionPlan, rollbackPluginInstance, switchPluginVersion } from '@/api'
import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/operation'
import { notifyPluginError } from '../utils/actions'
import { currentPluginInstance } from '../utils/catalog'

interface VersionContext extends PluginRefreshContext {
  catalog: Ref<InstalledPlugin[]>
  instances: Ref<PluginInstance[]>
  busyInstanceId: Ref<string>
  onApplied: () => void
  onConfigurationRequired: (instance: PluginInstance, artifact: PluginArtifact, plan: PluginVersionPlan, error: string) => void
}

export function usePluginVersions({ catalog, instances, busyInstanceId, refresh, onApplied, onConfigurationRequired }: VersionContext) {
  const showRollback = shallowRef(false)
  const rollbackInstance = shallowRef<PluginInstance | null>(null)
  const rollbackPlan = shallowRef<PluginRollbackPlan | null>(null)
  const loadingRollback = shallowRef(false)

  const pendingVersionSwitch = shallowRef<{ instance: PluginInstance, artifact: PluginArtifact, currentVersion: string } | null>(null)
  const showVersionSwitch = shallowRef(false)
  let rollbackController: AbortController | undefined

  function requestVersionSwitch(artifact: PluginArtifact) {
    const plugin = catalog.value.find(plugin => plugin.id === artifact.metadata.pluginId)
    const instance = plugin && currentPluginInstance(plugin)
    if (!plugin || !instance || !artifact.acceptedAt || busyInstanceId.value || instance.artifactSha256 === artifact.metadata.sha256)
      return
    pendingVersionSwitch.value = { instance, artifact, currentVersion: plugin.artifact.metadata.version }
    showVersionSwitch.value = true
  }

  async function confirmVersionSwitch() {
    if (!showVersionSwitch.value || !pendingVersionSwitch.value || busyInstanceId.value)
      return
    const { instance, artifact } = pendingVersionSwitch.value
    await applyVersionSwitch(instance, artifact)
  }

  async function applyVersionSwitch(instance: PluginInstance, artifact: PluginArtifact) {
    busyInstanceId.value = instance.id
    try {
      await switchPluginVersion({
        id: instance.id,
        target: { artifactSha256: artifact.metadata.sha256, expectedRevision: instance.revision },
      }, { silent: true })
      showVersionSwitch.value = false
      toast.success(`已切换至 ${artifact.metadata.version}`)
      await refresh(true)
      onApplied()
    }
    catch (error) {
      await refresh(true, true)
      const current = instances.value.find(value => value.id === instance.id)
      if (error instanceof ApiError && error.status === 400 && current?.revision === instance.revision) {
        try {
          const plan = await getPluginVersionPlan(instance.id, artifact.metadata.sha256, { silent: true })
          if (plan.instanceRevision !== instance.revision)
            throw new Error('插件设置已变更，请刷新后重试')
          showVersionSwitch.value = false
          onConfigurationRequired(current, artifact, plan, errorMessage(error, '请调整不兼容的设置后重试'))
        }
        catch (planError) {
          notifyPluginError('版本设置加载失败', planError)
        }
      }
      else {
        notifyPluginError('版本切换失败', error)
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
        notifyPluginError('回退版本加载失败', error)
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
      toast.success(`已回退至 ${target.version}，并恢复对应设置`)
      await refresh(true)
    }
    catch (error) {
      notifyPluginError('插件版本回退失败', error)
    }
    finally {
      busyInstanceId.value = ''
    }
  }

  watch(showRollback, (open) => {
    if (!open) {
      rollbackController?.abort()
      rollbackController = undefined
    }
  })

  onScopeDispose(() => rollbackController?.abort())

  return { showRollback, rollbackInstance, rollbackPlan, loadingRollback, pendingVersionSwitch, showVersionSwitch, requestVersionSwitch, confirmVersionSwitch, applyVersionSwitch, openRollback, confirmRollback }
}
