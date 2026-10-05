import type { AccountGroup, ApiKey } from '@/api'
import { normalizeRgbaHexColor, toast } from '@codex-proxy/ui'

import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import {
  createAccountGroup,
  deleteAccountGroup,
  disableAccountGroup,
  enableAccountGroup,
  getAccountGroups,
  getApiKeys,
  updateAccountGroup,
} from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { useRequestState } from '@/composables/useRequestState'
import { errorMessage } from '@/utils/operation'
import { DEFAULT_ACCOUNT_GROUP_COLOR } from '../constants'

export interface AccountGroupFormValue {
  disableFast: boolean
  name: string
  description: string
  color: string
}

export function useAccountGroups() {
  const searchQuery = shallowRef('')
  const statusQuery = shallowRef('')
  const showFormModal = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const showBatchDeleteModal = shallowRef(false)
  const showDisableModal = shallowRef(false)
  const selectedIds = ref<Set<string>>(new Set())
  const editingGroup = shallowRef<AccountGroup | null>(null)
  const pendingDeleteGroup = shallowRef<AccountGroup | null>(null)
  const pendingDisableGroup = shallowRef<AccountGroup | null>(null)
  const deleteCount = shallowRef(0)
  const clientKeys = shallowRef<ApiKey[]>([])
  const referenceKeysRequest = useRequestState()
  const form = ref<AccountGroupFormValue>(emptyForm())
  const savingAction = useAsyncAction()
  const deletingAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const disablingAction = useAsyncAction()
  const updatingStatusGroups = useIdSet<string>()

  const query = usePagedQuery({
    initialPageSize: 20,
    load: ({ page, pageSize }, options) =>
      getAccountGroups({
        page,
        pageSize,
        search: searchQuery.value.trim() || undefined,
        enabled: statusQuery.value ? statusQuery.value === 'enabled' : undefined,
      }, options),
  })

  const groups = query.items
  const pagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))
  const saving = savingAction.loading
  const deleting = deletingAction.loading
  const batchDeleting = batchDeletingAction.loading
  const disabling = disablingAction.loading
  const updatingStatusGroupIds = updatingStatusGroups.ids
  const referencedKeyNames = computed(() => {
    const groupId = pendingDisableGroup.value?.id
    if (!groupId)
      return []
    return referenceKeyNamesFor(groupId)
  })
  watch([showBatchDeleteModal, batchDeleting], ([open, busy]) => {
    if (open && !busy)
      deleteCount.value = selectedIds.value.size
  })

  function referenceKeyNamesFor(groupId: string) {
    return clientKeys.value
      .filter(key => key.groups.some(group => group.id === groupId))
      .map(key => key.name || key.prefix)
  }

  async function loadReferenceKeys() {
    const requestId = referenceKeysRequest.start()
    const requestOptions = { signal: referenceKeysRequest.signal }
    try {
      const items: ApiKey[] = []
      let cursor: string | undefined
      do {
        const result = await getApiKeys({ limit: 200, cursor }, requestOptions)
        if (!referenceKeysRequest.isCurrent(requestId))
          return
        items.push(...result.items)
        cursor = result.nextCursor ?? undefined
      } while (cursor)
      clientKeys.value = items
    }
    catch {
      if (referenceKeysRequest.isCurrent(requestId))
        clientKeys.value = []
    }
    finally {
      referenceKeysRequest.finish(requestId)
    }
  }

  function openCreate() {
    editingGroup.value = null
    form.value = emptyForm()
    showFormModal.value = true
  }

  function openEdit(group: AccountGroup) {
    editingGroup.value = group
    form.value = {
      name: group.name,
      description: group.description ?? '',
      color: group.color,
      disableFast: group.disableFast,
    }
    showFormModal.value = true
  }

  async function save() {
    if (saving.value)
      return
    const name = form.value.name.trim()
    if (!name) {
      toast.warning('请输入分组名称')
      return
    }
    const color = normalizeRgbaHexColor(form.value.color)
    if (!color) {
      toast.warning('请选择有效的分组颜色')
      return
    }

    await savingAction.run(async () => {
      const updating = Boolean(editingGroup.value)
      const description = form.value.description.trim() || null
      const disableFast = form.value.disableFast
      if (editingGroup.value) {
        await updateAccountGroup({ id: editingGroup.value.id, name, description, color, disableFast })
      }
      else {
        await createAccountGroup({
          name,
          description,
          color,
          disableFast,
        })
      }
      showFormModal.value = false
      await Promise.all([query.execute(), loadReferenceKeys()])
      toast.success(updating ? '分组已更新' : '分组已创建')
    })
  }

  function requestToggle(group: AccountGroup) {
    if (group.enabled) {
      pendingDisableGroup.value = group
      showDisableModal.value = true
      return
    }
    void toggleStatus(group)
  }

  async function confirmDisable() {
    const group = pendingDisableGroup.value
    if (!group)
      return
    await disablingAction.run(async () => {
      await disableAccountGroup({ id: group.id })
      showDisableModal.value = false
      await query.execute()
      toast.success('分组已禁用')
    })
  }

  async function toggleStatus(group: AccountGroup) {
    await updatingStatusGroups.run(group.id, async () => {
      try {
        const mutation = group.enabled ? disableAccountGroup : enableAccountGroup
        await mutation({ id: group.id })
        await query.execute()
        toast.success(group.enabled ? '分组已禁用' : '分组已启用')
      }
      catch {}
    })
  }

  function requestDelete(group: AccountGroup) {
    pendingDeleteGroup.value = group
    showDeleteModal.value = true
  }

  async function confirmDelete() {
    const group = pendingDeleteGroup.value
    if (!group || deleting.value)
      return
    await deletingAction.run(async () => {
      await deleteAccountGroup({ id: group.id })
      const remaining = new Set(selectedIds.value)
      remaining.delete(group.id)
      selectedIds.value = remaining
      showDeleteModal.value = false
      await query.execute()
      toast.success('分组已删除')
    }, { onError: () => void query.execute() })
  }

  async function confirmBatchDelete() {
    if (batchDeleting.value || selectedIds.value.size === 0)
      return

    let deletedCount = 0
    await batchDeletingAction.run(async () => {
      const failures: unknown[] = []
      for (const groupId of [...selectedIds.value]) {
        try {
          await deleteAccountGroup({ id: groupId }, { silent: true })
          deletedCount += 1
          const remaining = new Set(selectedIds.value)
          remaining.delete(groupId)
          selectedIds.value = remaining
        }
        catch (error: unknown) {
          failures.push(error)
        }
      }

      if (failures.length > 0)
        throw failures[0]

      await query.execute()
      showBatchDeleteModal.value = false
      toast.success(`已删除 ${deletedCount} 个分组`)
    }, {
      errorText: false,
      onError: (error) => {
        void query.execute().catch(() => undefined)
        toast.error(
          deletedCount > 0
            ? `已删除 ${deletedCount} 个分组，其余未删除：${errorMessage(error, '操作失败')}`
            : errorMessage(error, '批量删除失败'),
        )
      },
    })
  }

  function handlePageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    query.page.value = 1
    void query.execute()
  }

  watch(searchQuery, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      query.page.value = 1
      void query.execute()
    }, 250)
    onCleanup(() => clearTimeout(timer))
  })
  watch(statusQuery, () => {
    query.page.value = 1
    void query.execute()
  })
  onMounted(() => {
    void Promise.all([query.execute(), loadReferenceKeys()])
  })

  return {
    groups,
    loading: query.loading,
    pagination,
    searchQuery,
    statusQuery,
    showFormModal,
    showDeleteModal,
    showBatchDeleteModal,
    showDisableModal,
    selectedIds,
    editingGroup,
    pendingDeleteGroup,
    pendingDisableGroup,
    deleteCount,
    form,
    saving,
    deleting,
    batchDeleting,
    disabling,
    updatingStatusGroupIds,
    referencedKeyNames,
    openCreate,
    openEdit,
    save,
    requestToggle,
    confirmDisable,
    requestDelete,
    confirmDelete,
    confirmBatchDelete,
    handlePageChange,
    handlePageSizeChange,
  }
}

function emptyForm(): AccountGroupFormValue {
  return { name: '', description: '', color: DEFAULT_ACCOUNT_GROUP_COLOR, disableFast: false }
}
