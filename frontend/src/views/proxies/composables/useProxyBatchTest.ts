import type { ProxyBatchItem } from '../utils/batch'
import type { OutboundProxyRecord, ProxyBatchResult } from '@/api'
import type { RequestOptions } from '@/api/request'
import { onScopeDispose, ref, shallowRef } from 'vue'
import { t } from '@/i18n'
import { proxyBatchFailure, proxyRequestFailure } from '../utils/batch'

export type ProxyTestExecutor = (proxies: OutboundProxyRecord[], options: RequestOptions, detectLocation: boolean) => Promise<ProxyBatchResult<OutboundProxyRecord>[]>

export function useProxyBatchTest(test: ProxyTestExecutor) {
  const items = ref<ProxyBatchItem[]>([])
  const running = shallowRef(false)
  const stopping = shallowRef(false)
  let disposed = false

  function stop() {
    stopping.value = true
  }

  function reset() {
    if (running.value)
      return
    items.value = []
    stopping.value = false
  }

  async function run(proxies: OutboundProxyRecord[]) {
    if (running.value || disposed)
      return
    const targets = [
      ...new Map(proxies.map(proxy => [proxy.id, proxy])).values(),
    ]
    items.value = targets.map(proxy => ({
      key: proxy.id,
      name: proxy.name,
      endpoint: proxy.endpoint,
      status: 'pending',
      message: '',
    }))
    running.value = true
    stopping.value = false
    try {
      // 每批两条与后端并发数一致，让进度和停止操作不必等待长队列
      for (let offset = 0; offset < targets.length; offset += 2) {
        if (stopping.value || disposed)
          break
        const group = targets.slice(offset, offset + 2)
        const currentItems = items.value.slice(offset, offset + 2)
        for (const item of currentItems)
          item.status = 'running'
        try {
          const results = await test(group, { silent: true, retry: false }, true)
          for (const [index, item] of currentItems.entries()) {
            const result = results[index]
            if (result?.status === 'succeeded' && result.data.lastTest) {
              const record = result.data
              item.record = record
              item.status = record.lastTest!.success ? 'succeeded' : 'failed'
              item.message = record.lastTest!.message || (record.lastTest!.success
                ? t('proxies.batchTest.connectionIsNormal')
                : t('proxies.region.connectionFailed'))
            }
            else {
              const failure = proxyBatchFailure(result)
              item.status = failure.status
              item.message = failure.message
              if (failure.stop)
                stop()
            }
          }
        }
        catch (error) {
          const failure = proxyRequestFailure(error)
          for (const item of currentItems) {
            item.status = failure.status
            item.message = failure.message
          }
          if (failure.stop)
            stop()
        }
      }
    }
    finally {
      for (const item of items.value) {
        if (item.status === 'pending') {
          item.status = 'skipped'
          item.message = t('proxies.batchTest.stopped')
        }
      }
      running.value = false
    }
  }

  onScopeDispose(() => {
    disposed = true
    stop()
  })

  return {
    items,
    running,
    stopping,
    run,
    stop,
    reset,
  }
}
