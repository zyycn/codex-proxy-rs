import type { AccountGroup } from '@/api'

import { onMounted, shallowRef } from 'vue'
import { getAccountGroups } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

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
    catch (error: unknown) {
      toast.error(errorMessage(error, '账号分组加载失败'))
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
