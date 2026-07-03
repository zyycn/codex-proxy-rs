// @env browser
import { useClipboard } from '@vueuse/core'
import { onMounted, ref, type Ref } from 'vue'

import { createApiKey, deleteApiKeys, getApiKeys, updateApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'

export function useApiKeyMutations(selectedIds: Ref<Set<string>>) {
  const { copy } = useClipboard()
  const loading = ref(true)
  const apiKeys = ref<any[]>([])
  const showCreateModal = ref(false)
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const showKeyModal = ref(false)
  const createdKey = ref('')
  const editingLabel = ref<{ id: string; value: string } | null>(null)
  const pendingDeleteKey = ref<any | null>(null)
  const creatingKeyAction = useAsyncAction()
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const savingLabelKeys = useIdSet<string>()
  const creatingKey = creatingKeyAction.loading
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids
  const savingLabelKeyIds = savingLabelKeys.ids

  const createForm = ref({
    name: '',
    label: '',
  })

  async function loadApiKeys() {
    try {
      loading.value = true
      const data = await getApiKeys()
      apiKeys.value = data.items
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
        showCreateModal.value = false
        showKeyModal.value = true
        createForm.value = { name: '', label: '' }

        await loadApiKeys()
        toast.success('API Key 创建成功')
      },
      { errorText: '创建失败' },
    )
  }

  function requestDeleteKey(key: any) {
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
    if (selectedIds.value.size === 0) return

    await batchDeletingAction.run(
      async () => {
        const deleteCount = selectedIds.value.size
        await deleteApiKeys({ ids: [...selectedIds.value] })
        selectedIds.value = new Set()
        showDeleteModal.value = false
        await loadApiKeys()
        toast.success(`已删除 ${deleteCount} 个 API Key`)
      },
      { errorText: '批量删除失败' },
    )
  }

  async function handleToggleStatus(key: any) {
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

  async function handleUpdateLabel(keyId: string, label: string) {
    await savingLabelKeys.run(keyId, async () => {
      try {
        await updateApiKey({ id: keyId, label: label || null })
        editingLabel.value = null
        await loadApiKeys()
        toast.success('标签已更新')
      } catch (error: any) {
        toast.error(error.message || '标签更新失败')
      }
    })
  }

  function startEditLabel(key: any) {
    editingLabel.value = { id: key.id, value: key.label || '' }
  }

  function cancelEditLabel() {
    editingLabel.value = null
  }

  function currentEditingLabelValue() {
    return editingLabel.value?.value ?? ''
  }

  function submitEditingLabel(keyId: string) {
    void handleUpdateLabel(keyId, currentEditingLabelValue())
  }

  function updateEditingLabelValue(value: string) {
    if (!editingLabel.value) return
    editingLabel.value.value = value
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

  function maskKey(prefix: string): string {
    return `${prefix}••••••••••••••••`
  }

  onMounted(() => {
    loadApiKeys()
  })

  return {
    loading,
    apiKeys,
    showCreateModal,
    showDeleteModal,
    showSingleDeleteModal,
    showKeyModal,
    createdKey,
    editingLabel,
    pendingDeleteKey,
    creatingKey,
    deletingKey,
    batchDeleting,
    updatingStatusKeyIds,
    savingLabelKeyIds,
    createForm,
    handleCreate,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    startEditLabel,
    cancelEditLabel,
    currentEditingLabelValue,
    submitEditingLabel,
    updateEditingLabelValue,
    copyToClipboard,
    maskKey,
  }
}
