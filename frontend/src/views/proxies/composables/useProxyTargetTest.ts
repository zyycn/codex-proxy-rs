import type { OutboundProxyRecord, TargetProbeResult } from '@/api'
import { computed, ref } from 'vue'
import { testProxyTarget } from '@/api'
import { ApiError } from '@/api/request'
import { useRequestState } from '@/composables/useRequestState'
import { errorMessage } from '@/utils/operation'

export type TargetTestProxy = Pick<OutboundProxyRecord, 'id' | 'name'> & Partial<Pick<OutboundProxyRecord, 'endpoint' | 'detectedLocation' | 'lastTest'>>
export type ProxyTargetTestRow = Pick<TargetTestProxy, 'id' | 'name'> & { result: TargetProbeResult | null, error: string, running: boolean }

export function useProxyTargetTest(onStale: () => void) {
  const request = useRequestState()
  const rows = ref<ProxyTargetTestRow[]>([])

  async function start(proxies: TargetTestProxy[], targetName: string, revision: number) {
    rows.value = proxies.map(({ id, name }) => ({
      id,
      name,
      result: null,
      error: '',
      running: false,
    }))
    const run = request.start()
    let stop = false
    for (let offset = 0; offset < rows.value.length && !stop && request.isCurrent(run); offset += 2) {
      await Promise.all(rows.value.slice(offset, offset + 2).map(async (row) => {
        row.running = true
        try {
          const data = await testProxyTarget({ proxyId: row.id, targetName, revision }, { silent: true, signal: request.signal })
          if (request.isCurrent(run)) {
            row.name = data.proxyName
            row.result = data.result
          }
        }
        catch (cause) {
          if (!request.isCurrent(run))
            return
          row.error = errorMessage(cause)
          if (cause instanceof ApiError && ([401, 403, 409, 429].includes(cause.status) || cause.status >= 500 || cause.kind === 'network' || cause.kind === 'timeout')) {
            stop = true
            if (cause.status === 409)
              onStale()
          }
        }
        finally {
          if (request.isCurrent(run))
            row.running = false
        }
      }))
    }
    request.finish(run)
  }

  return {
    byProxyId: computed(() => new Map(rows.value.map(row => [row.id, row]))),
    clearResults: () => { rows.value = [] },
    running: request.loading,
    start,
    cancel: request.invalidate,
  }
}
