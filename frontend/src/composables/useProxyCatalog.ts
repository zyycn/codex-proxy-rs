import type { OutboundProxyRecord } from '@/api'
import { onMounted, shallowRef } from 'vue'
import { getProxies } from '@/api'

export function useProxyCatalog() {
  const proxies = shallowRef<OutboundProxyRecord[]>([])
  const loading = shallowRef(false)

  async function loadProxies() {
    if (loading.value)
      return
    loading.value = true
    try {
      const first = await getProxies({ page: 1, pageSize: 200 })
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page += 1) {
        items.push(...(await getProxies({ page, pageSize: 200 })).items)
      }
      proxies.value = items
    }
    catch {}
    finally {
      loading.value = false
    }
  }

  onMounted(() => void loadProxies())
  return { proxies, loading, loadProxies }
}
