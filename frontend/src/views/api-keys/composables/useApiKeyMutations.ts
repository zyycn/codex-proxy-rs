// @env browser
import { useClipboard } from '@vueuse/core'
import { onMounted, ref, type Ref } from 'vue'

import { createApiKey, deleteApiKeys, getApiKeys, updateApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'

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
  const creatingKey = ref(false)
  const deletingKey = ref(false)
  const batchDeleting = ref(false)
  const updatingStatusKeyIds = ref<Set<string>>(new Set())
  const savingLabelKeyIds = ref<Set<string>>(new Set())

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

    try {
      creatingKey.value = true
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
    } catch (error: any) {
      toast.error(error.message || '创建失败')
    } finally {
      creatingKey.value = false
    }
  }

  function requestDeleteKey(key: any) {
    pendingDeleteKey.value = key
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingKey.value) return

    const keyId = pendingDeleteKey.value?.id
    if (!keyId) return

    try {
      deletingKey.value = true
      await deleteApiKeys({ ids: [keyId] })
      showSingleDeleteModal.value = false
      pendingDeleteKey.value = null
      await loadApiKeys()
      toast.success('删除成功')
    } catch (error: any) {
      toast.error(error.message || '删除失败')
    } finally {
      deletingKey.value = false
    }
  }

  async function handleBatchDelete() {
    if (batchDeleting.value) return
    if (selectedIds.value.size === 0) return

    try {
      batchDeleting.value = true
      const deleteCount = selectedIds.value.size
      await deleteApiKeys({ ids: [...selectedIds.value] })
      selectedIds.value = new Set()
      showDeleteModal.value = false
      await loadApiKeys()
      toast.success(`已删除 ${deleteCount} 个 API Key`)
    } catch (error: any) {
      toast.error(error.message || '批量删除失败')
    } finally {
      batchDeleting.value = false
    }
  }

  async function handleToggleStatus(key: any) {
    if (updatingStatusKeyIds.value.has(key.id)) return
    updatingStatusKeyIds.value = new Set(updatingStatusKeyIds.value).add(key.id)
    try {
      await updateApiKey({ id: key.id, status: key.enabled ? 'disabled' : 'active' })
      await loadApiKeys()
      toast.success(key.enabled ? '已禁用' : '已启用')
    } catch (error: any) {
      toast.error(error.message || '状态更新失败')
    } finally {
      const next = new Set(updatingStatusKeyIds.value)
      next.delete(key.id)
      updatingStatusKeyIds.value = next
    }
  }

  async function handleUpdateLabel(keyId: string, label: string) {
    if (savingLabelKeyIds.value.has(keyId)) return
    savingLabelKeyIds.value = new Set(savingLabelKeyIds.value).add(keyId)
    try {
      await updateApiKey({ id: keyId, label: label || null })
      editingLabel.value = null
      await loadApiKeys()
      toast.success('标签已更新')
    } catch (error: any) {
      toast.error(error.message || '标签更新失败')
    } finally {
      const next = new Set(savingLabelKeyIds.value)
      next.delete(keyId)
      savingLabelKeyIds.value = next
    }
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
