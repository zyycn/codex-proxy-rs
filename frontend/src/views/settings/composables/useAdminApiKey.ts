import { reactive, shallowRef } from 'vue'

import {
  deleteAdminApiKey,
  getAdminApiKeyStatus,
  regenerateAdminApiKey,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'

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
        toast.success(wasEnabled ? '管理员 API Key 已更新' : '管理员 API Key 已生成')
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
        toast.success('管理员 API Key 已删除')
      },
    )
  }

  async function copyGeneratedKey() {
    await copyText(generatedKey.value, { successText: '已复制', errorFromException: true })
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
