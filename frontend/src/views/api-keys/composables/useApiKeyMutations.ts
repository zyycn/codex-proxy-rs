import type { Ref } from 'vue'
import type { getApiKeys } from '@/api'
import { useClipboard } from '@vueuse/core'

import { ref } from 'vue'
import { createApiKey, deleteApiKeys, updateApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage } from '@/utils/async'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

export function useApiKeyMutations(options: {
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const { copy } = useClipboard()
  const loadApiKeys = options.reload
  const showCreateModal = ref(false)
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const showKeyModal = ref(false)
  const createdKey = ref('')
  const createdKeyName = ref('')
  const pendingDeleteKey = ref<ApiKeyRow | null>(null)
  const creatingKeyAction = useAsyncAction()
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const creatingKey = creatingKeyAction.loading
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids

  const createForm = ref({
    name: '',
    label: '',
  })

  async function handleCreate() {
    if (creatingKey.value)
      return
    if (!createForm.value.name.trim()) {
      toast.warning('请输入 API Key 名称')
      return
    }

    await creatingKeyAction.run(
      async () => {
        const result = await createApiKey({
          name: createForm.value.name,
          label: createForm.value.label || undefined,
        })

        createdKey.value = result.key
        createdKeyName.value = result.name
        showCreateModal.value = false
        showKeyModal.value = true
        createForm.value = { name: '', label: '' }

        await loadApiKeys()
        toast.success('API Key 创建成功')
      },
      { errorText: '创建失败' },
    )
  }

  function requestDeleteKey(key: ApiKeyRow) {
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
        await deleteApiKeys({ ids: [keyId] })
        showSingleDeleteModal.value = false
        pendingDeleteKey.value = null
        await loadApiKeys()
        toast.success('删除成功')
      },
      { errorText: '删除失败' },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value)
      return
    if (options.selectedIds.value.size === 0)
      return

    await batchDeletingAction.run(
      async () => {
        const deleteCount = options.selectedIds.value.size
        await deleteApiKeys({ ids: [...options.selectedIds.value] })
        options.selectedIds.value = new Set()
        showDeleteModal.value = false
        await loadApiKeys()
        toast.success(`已删除 ${deleteCount} 个 API Key`)
      },
      { errorText: '批量删除失败' },
    )
  }

  async function handleToggleStatus(key: ApiKeyRow) {
    await updatingStatusKeys.run(key.id, async () => {
      try {
        await updateApiKey({ id: key.id, status: key.enabled ? 'disabled' : 'active' })
        await loadApiKeys()
        toast.success(key.enabled ? '已禁用' : '已启用')
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '状态更新失败'))
      }
    })
  }

  async function copyToClipboard(text: string) {
    if (!text) {
      toast.error('复制失败')
      return
    }

    try {
      await copy(text)
      toast.success('已复制到剪贴板')
    }
    catch {
      toast.error('复制失败')
    }
  }

  return {
    showCreateModal,
    showDeleteModal,
    showSingleDeleteModal,
    showKeyModal,
    createdKey,
    createdKeyName,
    pendingDeleteKey,
    creatingKey,
    deletingKey,
    batchDeleting,
    updatingStatusKeyIds,
    createForm,
    handleCreate,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    copyToClipboard,
  }
}
