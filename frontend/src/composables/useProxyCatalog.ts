import type { OutboundProxy } from '@/api'

import { onMounted, shallowRef } from 'vue'
import { getOutboundProxies } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

export function useProxyCatalog(options: { immediate?: boolean } = {}) {
  const proxies = shallowRef<OutboundProxy[]>([])
  const loading = shallowRef(false)

  async function loadProxies() {
    loading.value = true
    try {
      const first = await getOutboundProxies({ page: 1, pageSize: 200 })
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page += 1) {
        const result = await getOutboundProxies({ page, pageSize: first.page.pageSize })
        items.push(...result.items)
      }
      proxies.value = items
      return items
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '代理列表加载失败'))
      return []
    }
    finally {
      loading.value = false
    }
  }

  if (options.immediate !== false) {
    onMounted(() => {
      void loadProxies()
    })
  }

  return {
    proxies,
    loading,
    loadProxies,
  }
}
