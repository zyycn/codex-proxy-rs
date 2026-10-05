import type { PluginRefreshContext } from '../utils/actions'
import type { PluginArtifact } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { deletePluginArtifact } from '@/api'
import { notifyPluginError } from '../utils/actions'

export function usePluginArtifactActions({ refresh }: PluginRefreshContext) {
  const showArtifactDelete = shallowRef(false)
  const pendingArtifact = shallowRef<PluginArtifact | null>(null)
  const busyDigest = shallowRef('')

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
      toast.success('插件制品已删除')
      await refresh(true)
    }
    catch (error) {
      notifyPluginError('插件制品删除失败', error)
    }
    finally {
      busyDigest.value = ''
    }
  }

  return { showArtifactDelete, pendingArtifact, busyDigest, requestArtifactDelete, confirmArtifactDelete }
}
