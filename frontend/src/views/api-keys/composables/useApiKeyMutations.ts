import type { Ref } from 'vue'
import type { getApiKeys } from '@/api'
import { ref, shallowRef, watch } from 'vue'
import {
  createApiKey,
  deleteApiKey,
  disableApiKey,
  enableApiKey,
  revealApiKey,
  updateApiKey,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { useIdSet } from '@/composables/useIdSet'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

export interface ApiKeyFormValue {
  name: string
  label: string
  groupIds: string[]
  maxConcurrency: string
  requestsPerMinute: string
  dailyLimitUsd: string
  weeklyLimitUsd: string
}

export function useApiKeyMutations(options: {
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const copyText = useCopyText()
  const showFormModal = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const showSingleDeleteModal = shallowRef(false)
  const showKeyModal = shallowRef(false)
  const showAllAccountsConfirm = shallowRef(false)
  const createdKey = shallowRef('')
  const createdKeyName = shallowRef('')
  const editingKey = shallowRef<ApiKeyRow | null>(null)
  const pendingDeleteKey = shallowRef<ApiKeyRow | null>(null)
  const savingKeyAction = useAsyncAction()
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const revealingKeys = useIdSet<string>()
  const savingKey = savingKeyAction.loading
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids
  const revealingKeyIds = revealingKeys.ids
  const form = ref<ApiKeyFormValue>(emptyForm())

  function openCreate() {
    editingKey.value = null
    form.value = emptyForm()
    showFormModal.value = true
  }

  function openEdit(key: ApiKeyRow) {
    editingKey.value = key
    form.value = {
      name: key.name,
      label: key.label ?? '',
      groupIds: key.groups.map(group => group.id),
      maxConcurrency: limitInputValue(key.maxConcurrency),
      requestsPerMinute: limitInputValue(key.requestsPerMinute),
      dailyLimitUsd: limitInputValue(key.dailyLimitUsd),
      weeklyLimitUsd: limitInputValue(key.weeklyLimitUsd),
    }
    showFormModal.value = true
  }

  function requestSave() {
    if (!validateForm() || savingKey.value)
      return
    if (form.value.groupIds.length === 0) {
      showAllAccountsConfirm.value = true
      return
    }
    void save()
  }

  async function confirmAllAccountsScope() {
    showAllAccountsConfirm.value = false
    await save()
  }

  async function save() {
    if (!validateForm() || savingKey.value)
      return

    await savingKeyAction.run(
      async () => {
        const payload = {
          name: form.value.name.trim(),
          label: form.value.label.trim() || null,
          groupIds: [...new Set(form.value.groupIds)],
          maxConcurrency: parseLimit(form.value.maxConcurrency),
          requestsPerMinute: parseLimit(form.value.requestsPerMinute),
          dailyLimitUsd: form.value.dailyLimitUsd.trim() || '0',
          weeklyLimitUsd: form.value.weeklyLimitUsd.trim() || '0',
        }
        const current = editingKey.value
        if (current) {
          await updateApiKey({ id: current.id, ...payload })
        }
        else {
          const result = await createApiKey(payload)
          createdKey.value = result.plaintextKey
          createdKeyName.value = payload.name
        }

        showFormModal.value = false
        editingKey.value = null
        form.value = emptyForm()
        await options.reload()
        if (current) {
          toast.success('API Key 已更新')
        }
        else {
          showKeyModal.value = true
          toast.success('API Key 创建成功')
        }
      },
      { onError: () => void options.reload() },
    )
  }

  function validateForm() {
    for (const [label, value] of [['日限额', form.value.dailyLimitUsd], ['周限额', form.value.weeklyLimitUsd]]) {
      if (value.trim() && !/^\d{1,10}(?:\.\d{1,10})?$/.test(value.trim())) {
        toast.warning(`${label}必须是非负金额，最多 10 位小数`)
        return false
      }
    }
    if (!form.value.name.trim()) {
      toast.warning('请输入 API Key 名称')
      return false
    }
    for (const [label, value] of [
      ['最大并发', form.value.maxConcurrency],
      ['每分钟请求数', form.value.requestsPerMinute],
    ] as const) {
      const parsed = Number(value)
      if (!Number.isSafeInteger(parsed) || parsed < 0) {
        toast.warning(`${label}必须是非负整数`)
        return false
      }
    }
    return true
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
        await deleteApiKey({ id: keyId })
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(keyId)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        pendingDeleteKey.value = null
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

  async function handleToggleStatus(key: ApiKeyRow) {
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

  async function copyToClipboard(text: string) {
    await copyText(text, { successText: '已复制到剪贴板', emptyErrorText: '复制失败' })
  }

  async function revealPlaintextKey(apiKey: ApiKeyRow) {
    try {
      const result = await revealingKeys.run(apiKey.id, () => revealApiKey({ id: apiKey.id }))
      if (!result)
        return undefined
      if (!result.plaintextKey) {
        toast.error('完整 API Key 不可用')
        return undefined
      }
      return result.plaintextKey
    }
    catch {
      return undefined
    }
  }

  async function copyApiKey(apiKey: ApiKeyRow) {
    const key = await revealPlaintextKey(apiKey)
    if (key)
      await copyToClipboard(key)
  }

  watch(showKeyModal, (open) => {
    if (!open) {
      createdKey.value = ''
      createdKeyName.value = ''
    }
  })
  watch(showFormModal, (open) => {
    if (!open && !savingKey.value) {
      editingKey.value = null
      form.value = emptyForm()
    }
  })

  return {
    showFormModal,
    showDeleteModal,
    showSingleDeleteModal,
    showKeyModal,
    showAllAccountsConfirm,
    createdKey,
    createdKeyName,
    editingKey,
    pendingDeleteKey,
    savingKey,
    deletingKey,
    batchDeleting,
    updatingStatusKeyIds,
    revealingKeyIds,
    form,
    openCreate,
    openEdit,
    requestSave,
    confirmAllAccountsScope,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    copyToClipboard,
    revealPlaintextKey,
    copyApiKey,
  }
}

function emptyForm(): ApiKeyFormValue {
  return {
    name: '',
    label: '',
    groupIds: [],
    maxConcurrency: '',
    requestsPerMinute: '',
    dailyLimitUsd: '',
    weeklyLimitUsd: '',
  }
}

function limitInputValue(limit: string | number) {
  return Number(limit) === 0 ? '' : String(limit)
}

function parseLimit(value: string) {
  return Number(value)
}
