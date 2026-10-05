import type { OutboundProxyRecord } from '@/api'
import { onMounted, shallowRef } from 'vue'
import { getProxies } from '@/api'
import { useRequestState } from './useRequestState'

export function useProxyCatalog() {
  const proxies = shallowRef<OutboundProxyRecord[]>([])
  const request = useRequestState()
  const { loading } = request

  async function loadProxies() {
    if (loading.value)
      return
    const requestId = request.start()
    const requestOptions = { signal: request.signal }
    try {
      const first = await getProxies({ page: 1, pageSize: 200 }, requestOptions)
      if (!request.isCurrent(requestId))
        return
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page += 1) {
        const result = await getProxies({ page, pageSize: first.page.pageSize }, requestOptions)
        if (!request.isCurrent(requestId))
          return
        items.push(...result.items)
      }
      proxies.value = items
    }
    catch {}
    finally {
      request.finish(requestId)
    }
  }

  onMounted(() => void loadProxies())
  return { proxies, loading, loadProxies }
}
