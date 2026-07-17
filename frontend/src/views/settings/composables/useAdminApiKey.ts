import { useClipboard } from '@vueuse/core'
import { onMounted, reactive, shallowRef } from 'vue'

import {
  deleteAdminApiKey,
  getAdminApiKeyStatus,
  regenerateAdminApiKey,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

export function useAdminApiKey() {
  const loading = shallowRef(true)
  const regenerating = shallowRef(false)
  const deleting = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const generatedKey = shallowRef('')
  const status = reactive({ exists: false })
  const { copy } = useClipboard()

  async function loadStatus() {
    try {
      loading.value = true
      status.exists = (await getAdminApiKeyStatus()).exists
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '管理员 API Key 状态加载失败'))
    }
    finally {
      loading.value = false
    }
  }

  async function regenerate() {
    if (regenerating.value || deleting.value)
      return

    try {
      regenerating.value = true
      const wasEnabled = status.exists
      generatedKey.value = (await regenerateAdminApiKey()).key
      status.exists = true
      toast.success(wasEnabled ? '管理员 API Key 已更新' : '管理员 API Key 已生成')
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '生成失败'))
    }
    finally {
      regenerating.value = false
    }
  }

  async function remove() {
    if (deleting.value || regenerating.value)
      return

    try {
      deleting.value = true
      await deleteAdminApiKey()
      status.exists = false
      generatedKey.value = ''
      showDeleteModal.value = false
      toast.success('管理员 API Key 已删除')
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '删除失败'))
    }
    finally {
      deleting.value = false
    }
  }

  async function copyGeneratedKey() {
    if (!generatedKey.value)
      return

    try {
      await copy(generatedKey.value)
      toast.success('已复制')
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '复制失败'))
    }
  }

  onMounted(() => {
    void loadStatus()
  })

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
  }
}
