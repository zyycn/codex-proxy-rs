import type { AccountGroup } from '@/api'

import { onMounted, shallowRef } from 'vue'
import { getAccountGroups } from '@/api'
import { useRequestState } from './useRequestState'

export function useAccountGroupCatalog(options: { immediate?: boolean } = {}) {
  const groups = shallowRef<AccountGroup[]>([])
  const request = useRequestState()
  const { loading } = request

  async function loadGroups() {
    const requestId = request.start()
    const requestOptions = { signal: request.signal }
    try {
      const first = await getAccountGroups({ page: 1, pageSize: 200 }, requestOptions)
      if (!request.isCurrent(requestId))
        return []
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page += 1) {
        const result = await getAccountGroups({ page, pageSize: first.page.pageSize }, requestOptions)
        if (!request.isCurrent(requestId))
          return []
        items.push(...result.items)
      }
      groups.value = items
      return items
    }
    catch {
      return []
    }
    finally {
      request.finish(requestId)
    }
  }

  if (options.immediate !== false) {
    onMounted(() => {
      void loadGroups()
    })
  }

  return {
    groups,
    loading,
    loadGroups,
  }
}
