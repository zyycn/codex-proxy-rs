import type { Ref } from 'vue'
import type { ApiKey } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef, watch } from 'vue'
import { deleteApiKey, disableApiKey, enableApiKey } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'

export function useApiKeyMutations(options: {
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const showDeleteModal = shallowRef(false)
  const showSingleDeleteModal = shallowRef(false)
  const pendingDeleteKey = shallowRef<ApiKey | null>(null)
  const deleteCount = shallowRef(0)
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids

  watch([showDeleteModal, batchDeleting], ([open, busy]) => {
    if (open && !busy)
      deleteCount.value = options.selectedIds.value.size
  })

  function requestDeleteKey(key: ApiKey) {
    pendingDeleteKey.value = key
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingKey.value)
      return
    const keyId = pendingDeleteKey.value?.id
    if (!keyId)
      return

    await deletingKeyAction.run(
      async () => {
        await deleteApiKey({ id: keyId })
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(keyId)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        await options.reload()
        toast.success('删除成功')
      },
      { onError: () => void options.reload() },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value || options.selectedIds.value.size === 0)
      return

    await batchDeletingAction.run(
      async () => {
        const deleteCount = options.selectedIds.value.size
        for (const keyId of [...options.selectedIds.value]) {
          await deleteApiKey({ id: keyId })
          const remaining = new Set(options.selectedIds.value)
          remaining.delete(keyId)
          options.selectedIds.value = remaining
        }
        showDeleteModal.value = false
        await options.reload()
        toast.success(`已删除 ${deleteCount} 个 API Key`)
      },
      { onError: () => void options.reload() },
    )
  }

  async function handleToggleStatus(key: ApiKey) {
    await updatingStatusKeys.run(key.id, async () => {
      try {
        const mutation = key.enabled ? disableApiKey : enableApiKey
        await mutation({ id: key.id })
        await options.reload()
        toast.success(key.enabled ? '已禁用' : '已启用')
      }
      catch {
        void options.reload()
      }
    })
  }

  return { showDeleteModal, showSingleDeleteModal, pendingDeleteKey, deleteCount, deletingKey, batchDeleting, updatingStatusKeyIds, requestDeleteKey, handleDelete, handleBatchDelete, handleToggleStatus }
}
