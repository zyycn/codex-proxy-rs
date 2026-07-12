import { useClipboard } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { onMounted, ref, type Ref } from 'vue'

import { createApiKey, deleteApiKeys, getApiKeys, updateApiKey } from '@/api'
import type { ClientApiKey } from '@/api/modules/api-keys'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'

export function useApiKeyMutations(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  selectedIds: Ref<Set<string>>
  totalApiKeys: Ref<number>
}) {
  const { copy } = useClipboard()
  const loading = ref(true)
  const apiKeys = ref<ClientApiKey[]>([])
  const showCreateModal = ref(false)
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const showKeyModal = ref(false)
  const createdKey = ref('')
  const createdKeyName = ref('')
  const pendingDeleteKey = ref<ClientApiKey | null>(null)
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

  async function loadApiKeys() {
    try {
      loading.value = true
      const result = await getApiKeys({
        page: options.page.value,
        pageSize: options.pageSize.value,
        search: options.searchQuery.value.trim() || undefined,
      })
      apiKeys.value = result.items
      options.page.value = result.page.page
      options.pageSize.value = result.page.pageSize
      options.totalApiKeys.value = result.page.total

      if (apiKeys.value.length === 0 && result.page.total > 0 && result.page.page > 1) {
        options.page.value = clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY)
        await loadApiKeys()
      }
    } catch (error: any) {
      toast.error(error.message || '加载失败')
    } finally {
      loading.value = false
    }
  }

  async function handleCreate() {
    if (creatingKey.value) return
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

  function requestDeleteKey(key: ClientApiKey) {
    pendingDeleteKey.value = key
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingKey.value) return

    const keyId = pendingDeleteKey.value?.id
    if (!keyId) return

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
    if (batchDeleting.value) return
    if (options.selectedIds.value.size === 0) return

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

  async function handleToggleStatus(key: ClientApiKey) {
    await updatingStatusKeys.run(key.id, async () => {
      try {
        await updateApiKey({ id: key.id, status: key.enabled ? 'disabled' : 'active' })
        await loadApiKeys()
        toast.success(key.enabled ? '已禁用' : '已启用')
      } catch (error: any) {
        toast.error(error.message || '状态更新失败')
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
    } catch {
      toast.error('复制失败')
    }
  }

  onMounted(() => {
    void loadApiKeys()
  })

  return {
    loading,
    apiKeys,
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
    loadApiKeys,
    handleCreate,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    copyToClipboard,
  }
}
