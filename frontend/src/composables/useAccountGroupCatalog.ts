import type { AccountGroup } from '@/api'

import { onMounted, shallowRef } from 'vue'
import { getAccountGroups } from '@/api'

export function useAccountGroupCatalog(options: { immediate?: boolean } = {}) {
  const groups = shallowRef<AccountGroup[]>([])
  const loading = shallowRef(false)

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
    catch {
      return []
    }
    finally {
      loading.value = false
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
