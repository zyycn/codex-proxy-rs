import type { PluginRefreshContext } from '../utils/actions'
import type { PluginArtifact } from '@/api'
import { shallowRef } from 'vue'
import { deletePluginArtifact } from '@/api'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
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
      ZNotification.success({
        message: t('plugins.artifacts.pluginProductHasBeenDeleted'),
      })
      await refresh(true)
    }
    catch (error) {
      notifyPluginError(t('plugins.artifacts.pluginProductDeletionFailed'), error)
    }
    finally {
      busyDigest.value = ''
    }
  }

  return {
    showArtifactDelete,
    pendingArtifact,
    busyDigest,
    requestArtifactDelete,
    confirmArtifactDelete,
  }
}
