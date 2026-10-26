import { reactive, shallowRef } from 'vue'
import {
  deleteAdminApiKey,
  getAdminApiKeyStatus,
  regenerateAdminApiKey,
} from '@/api'

import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'

export function useAdminApiKey() {
  const loading = shallowRef(true)
  const showDeleteModal = shallowRef(false)
  const generatedKey = shallowRef('')
  const status = reactive({ exists: false })
  const copyText = useCopyText()
  const regenerateAction = useAsyncAction()
  const removeAction = useAsyncAction()
  const regenerating = regenerateAction.loading
  const deleting = removeAction.loading

  async function loadStatus() {
    try {
      loading.value = true
      status.exists = (await getAdminApiKeyStatus()).exists
    }
    catch {}
    finally {
      loading.value = false
    }
  }

  async function regenerate() {
    if (regenerating.value || deleting.value)
      return

    await regenerateAction.run(
      async () => {
        const wasEnabled = status.exists
        generatedKey.value = (await regenerateAdminApiKey()).key
        status.exists = true
        ZNotification.success({
          message: wasEnabled
            ? t('settings.adminApiKey.adminApiKeyUpdated')
            : t('settings.adminApiKey.generated'),
        })
      },
    )
  }

  async function remove() {
    if (deleting.value || regenerating.value)
      return

    await removeAction.run(
      async () => {
        await deleteAdminApiKey()
        status.exists = false
        generatedKey.value = ''
        showDeleteModal.value = false
        ZNotification.success({
          message: t('settings.adminApiKey.adminApiKeyDeleted'),
        })
      },
    )
  }

  async function copyGeneratedKey() {
    await copyText(generatedKey.value, {
      successText: t('common.feedback.copied'),
      errorFromException: true,
    })
  }

  return {
    loading,
    regenerating,
    deleting,
    showDeleteModal,
    generatedKey,
    status,
    regenerate,
    remove,
    copyGeneratedKey,
    loadStatus,
  }
}
