import type { Ref } from 'vue'
import type { getApiKeys } from '@/api'
import { useClipboard } from '@vueuse/core'

import { ref, shallowRef, watch } from 'vue'
import { createApiKey, deleteApiKey, disableApiKey, enableApiKey, revealApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage } from '@/utils/async'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

export function useApiKeyMutations(options: {
  selectedIds: Ref<Set<string>>
  configRevision: Ref<number>
  reload: () => Promise<unknown>
}) {
  const { copy } = useClipboard()
  const loadApiKeys = options.reload
  const showCreateModal = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const showSingleDeleteModal = shallowRef(false)
  const showKeyModal = shallowRef(false)
  const createdKey = shallowRef('')
  const createdKeyName = shallowRef('')
  const pendingDeleteKey = shallowRef<ApiKeyRow | null>(null)
  const creatingKeyAction = useAsyncAction()
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const revealingKeys = useIdSet<string>()
  const creatingKey = creatingKeyAction.loading
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids
  const revealingKeyIds = revealingKeys.ids

  const createForm = ref({
    name: '',
    label: '',
    providerKind: 'openai',
  })

  watch(showKeyModal, (open) => {
    if (!open) {
      createdKey.value = ''
      createdKeyName.value = ''
    }
  })

  function currentRevision() {
    if (options.configRevision.value <= 0) {
      throw new Error('配置版本尚未加载，请刷新后重试')
    }
    return options.configRevision.value
  }

  async function handleCreate() {
    if (creatingKey.value)
      return
    if (!createForm.value.name.trim()) {
      toast.warning('请输入 API Key 名称')
      return
    }

    await creatingKeyAction.run(
      async () => {
        const name = createForm.value.name.trim()
        const result = await createApiKey({
          expectedConfigRevision: currentRevision(),
          name,
          label: createForm.value.label.trim() || undefined,
          providerKind: createForm.value.providerKind,
          maxConcurrency: 0,
          requestsPerMinute: 0,
          tokensPerMinute: 0,
        })

        options.configRevision.value = result.configRevision
        createdKey.value = result.plaintextKey
        createdKeyName.value = name
        showCreateModal.value = false
        showKeyModal.value = true
        createForm.value = { name: '', label: '', providerKind: 'openai' }

        await loadApiKeys()
        toast.success('API Key 创建成功')
      },
      { errorText: '创建失败', onError: () => void loadApiKeys() },
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
        await deleteKey(keyId)
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(keyId)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        pendingDeleteKey.value = null
        await loadApiKeys()
        toast.success('删除成功')
      },
      { errorText: '删除失败', onError: () => void loadApiKeys() },
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
        for (const keyId of [...options.selectedIds.value]) {
          await deleteKey(keyId)
          const remaining = new Set(options.selectedIds.value)
          remaining.delete(keyId)
          options.selectedIds.value = remaining
        }
        showDeleteModal.value = false
        await loadApiKeys()
        toast.success(`已删除 ${deleteCount} 个 API Key`)
      },
      { errorText: '批量删除失败', onError: () => void loadApiKeys() },
    )
  }

  async function deleteKey(keyId: string) {
    const deleted = await deleteApiKey({
      id: keyId,
      expectedConfigRevision: currentRevision(),
    })
    options.configRevision.value = deleted.configRevision
  }

  async function handleToggleStatus(key: ApiKeyRow) {
    await updatingStatusKeys.run(key.id, async () => {
      try {
        const mutation = key.enabled ? disableApiKey : enableApiKey
        const result = await mutation({
          id: key.id,
          expectedConfigRevision: currentRevision(),
        })
        options.configRevision.value = result.configRevision
        await loadApiKeys()
        toast.success(key.enabled ? '已禁用' : '已启用')
      }
      catch (error: unknown) {
        void loadApiKeys()
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

  async function revealPlaintextKey(apiKey: ApiKeyRow) {
    const result = await revealingKeys.run(apiKey.id, () => revealApiKey({ id: apiKey.id }))
    if (!result?.plaintextKey)
      throw new Error('完整 API Key 不可用')
    return result.plaintextKey
  }

  async function copyApiKey(apiKey: ApiKeyRow) {
    try {
      await copyToClipboard(await revealPlaintextKey(apiKey))
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '读取完整密钥失败'))
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
    revealingKeyIds,
    createForm,
    handleCreate,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    copyToClipboard,
    revealPlaintextKey,
    copyApiKey,
  }
}
