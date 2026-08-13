import type { AccountGroup, AccountGroupMember } from '@/api'

import { onMounted, shallowRef } from 'vue'
import { getAccountGroupMembers, getAccountGroups } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

export function useAccountGroupCatalog(options: { immediate?: boolean } = {}) {
  const groups = shallowRef<AccountGroup[]>([])
  const membersByGroupId = shallowRef<Record<string, AccountGroupMember[]>>({})
  const loading = shallowRef(false)
  const loadingMemberIds = shallowRef<ReadonlySet<string>>(new Set())
  const memberRequests = new Map<string, Promise<AccountGroupMember[]>>()

  async function loadGroups() {
    loading.value = true
    try {
      const first = await getAccountGroups({ page: 1, pageSize: 200 })
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page += 1) {
        const result = await getAccountGroups({ page, pageSize: first.page.pageSize })
        items.push(...result.items)
      }
      groups.value = items
      return items
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '账号分组加载失败'))
      return []
    }
    finally {
      loading.value = false
    }
  }

  async function loadMembers(groupId: string, force = false) {
    if (!force && Object.hasOwn(membersByGroupId.value, groupId))
      return membersByGroupId.value[groupId] ?? []

    const existing = memberRequests.get(groupId)
    if (existing)
      return existing

    const request = (async () => {
      setMemberLoading(groupId, true)
      try {
        const result = await getAccountGroupMembers({ id: groupId })
        membersByGroupId.value = {
          ...membersByGroupId.value,
          [groupId]: result.items,
        }
        return result.items
      }
      finally {
        setMemberLoading(groupId, false)
        memberRequests.delete(groupId)
      }
    })()
    memberRequests.set(groupId, request)
    return request
  }

  async function ensureMembers(groupIds: string[]) {
    try {
      await Promise.all([...new Set(groupIds)].map(id => loadMembers(id)))
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '分组成员加载失败'))
    }
  }

  function setMemberLoading(groupId: string, value: boolean) {
    const next = new Set(loadingMemberIds.value)
    if (value)
      next.add(groupId)
    else
      next.delete(groupId)
    loadingMemberIds.value = next
  }

  if (options.immediate !== false) {
    onMounted(() => {
      void loadGroups()
    })
  }

  return {
    groups,
    membersByGroupId,
    loading,
    loadingMemberIds,
    loadGroups,
    loadMembers,
    ensureMembers,
  }
}
