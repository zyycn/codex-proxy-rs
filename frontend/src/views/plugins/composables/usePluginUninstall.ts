import type { InstalledPlugin } from '../utils/catalog'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { deletePluginArtifact, deletePluginInstance, disablePluginInstance, getPluginArtifacts, getPluginInstances } from '@/api'
import { notifyPluginError } from '../utils/actions'

export function usePluginUninstall(refresh: () => Promise<void>) {
  const open = shallowRef(false)
  const pending = shallowRef<InstalledPlugin | null>(null)
  const acknowledged = shallowRef(false)
  const busy = shallowRef(false)
  const progress = shallowRef('')

  function request(plugin: InstalledPlugin) {
    // 固定确认范围，重试不扩大到此后新建的配置或版本。
    pending.value = { ...plugin, artifacts: [...plugin.artifacts], configurations: [...plugin.configurations] }
    acknowledged.value = false
    progress.value = ''
    open.value = true
  }

  async function confirm() {
    const plugin = pending.value
    if (!plugin || !acknowledged.value || busy.value)
      return
    busy.value = true
    const options = { silent: true }
    try {
      const current = await getPluginInstances(options)
      const digests = new Set(plugin.artifacts.map(artifact => artifact.metadata.sha256))
      const ids = new Set(plugin.configurations.map(instance => instance.id))
      if (current.some(instance => digests.has(instance.artifactSha256) && !ids.has(instance.id)))
        throw new Error('出现了新的插件配置，请关闭并重新确认卸载范围')
      for (const configured of plugin.configurations) {
        const instance = current.find(value => value.id === configured.id)
        if (!instance)
          continue
        if (instance.artifactSha256 !== configured.artifactSha256)
          throw new Error(`“${instance.name}”已切换版本，请重新确认卸载范围`)
        progress.value = `正在停用并删除配置：${instance.name}`
        if (instance.enabled)
          await disablePluginInstance({ id: instance.id }, options)
        await deletePluginInstance({ id: instance.id }, options)
      }
      const remaining = await getPluginArtifacts(options)
      for (const artifact of plugin.artifacts) {
        if (!remaining.some(value => value.metadata.sha256 === artifact.metadata.sha256))
          continue
        progress.value = `正在删除版本：${artifact.metadata.version}`
        await deletePluginArtifact({ sha256: artifact.metadata.sha256 }, options)
      }
      if ((await getPluginArtifacts(options)).some(artifact => artifact.metadata.pluginId === plugin.id))
        throw new Error('已删除确认范围，但还有新安装的版本，请关闭并重新确认剩余内容')
      open.value = false
      toast.success('插件已卸载，相关数据与来源设置已清理')
      await refresh()
    }
    catch (cause) {
      // 卸载不是跨请求事务，刷新已完成的步骤后提示失败原因，可从剩余部分重试。
      await refresh()
      notifyPluginError('卸载尚未完成，可重试剩余步骤', cause)
    }
    finally {
      busy.value = false
      if (open.value)
        progress.value = ''
    }
  }
  return { open, pending, acknowledged, busy, progress, request, confirm }
}
