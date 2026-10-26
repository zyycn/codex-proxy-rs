import type { ProxyBatchItem } from '../utils/batch'
import type { ProxyTestExecutor } from './useProxyBatchTest'
import { computed, onScopeDispose, ref, shallowRef } from 'vue'
import { createProxy } from '@/api'
import { t } from '@/i18n'
import { MAX_BATCH_INPUT_BYTES, parseProxyBatch, proxyBatchFailure, proxyBatchInputError, proxyRequestFailure } from '../utils/batch'
import { useProxyBatchTest } from './useProxyBatchTest'

interface CreateItem extends ProxyBatchItem {
  input: string
  proxyUrl: string
}

export function useProxyBatchCreate(options: { test: ProxyTestExecutor, reload: () => Promise<unknown> }) {
  const text = shallowRef('')
  const prefix = shallowRef(t('proxies.batchCreate.proxy'))
  const protocol = shallowRef('http')
  const items = ref<CreateItem[]>([])
  const busy = shallowRef(false)
  const stopRequested = shallowRef(false)
  const testingRequested = shallowRef(false)
  const tests = useProxyBatchTest(options.test)
  const stopping = computed(() => stopRequested.value || tests.stopping.value)
  const parsed = computed(() => text.value.length > MAX_BATCH_INPUT_BYTES
    ? []
    : parseProxyBatch(text.value, prefix.value, protocol.value))
  const inputError = computed(() => proxyBatchInputError(text.value, parsed.value.length))
  const created = computed(() => items.value.flatMap(item => item.record
    ? [item.record]
    : []))
  const canEditFailures = computed(() => items.value.some(item => !item.record && item.status !== 'unknown' && Boolean(item.input)))
  let disposed = false

  function stop() {
    stopRequested.value = true
    tests.stop()
  }

  function clear() {
    if (busy.value)
      return
    text.value = ''
    prefix.value = t('proxies.batchCreate.proxy')
    protocol.value = 'http'
    items.value = []
    testingRequested.value = false
    stopRequested.value = false
    tests.reset()
  }

  function editFailures() {
    if (busy.value)
      return
    text.value = items.value.filter(item => !item.record && item.status !== 'unknown').map(item => item.input).filter(Boolean).join('\n')
    items.value = []
    testingRequested.value = false
    stopRequested.value = false
    tests.reset()
  }

  async function submit(testAfterCreate: boolean) {
    if (busy.value || disposed || inputError.value || !parsed.value.length)
      return
    items.value = parsed.value.map(row => ({
      key: String(row.line),
      name: row.name,
      endpoint: row.endpoint,
      input: row.raw,
      proxyUrl: row.proxyUrl,
      status: row.error ? 'failed' : 'pending',
      message: row.error,
    }))
    // 开始后只保留逐行输入，成功项立即清除认证，避免原始文本保留另一份明文
    text.value = ''
    busy.value = true
    stopRequested.value = false
    testingRequested.value = testAfterCreate
    try {
      const targets = items.value.filter(item => item.status === 'pending')
      for (let offset = 0; offset < targets.length; offset += 10) {
        if (stopRequested.value || disposed)
          break
        const group = targets.slice(offset, offset + 10)
        for (const item of group)
          item.status = 'running'
        try {
          const results = await createProxy(group.map(item => ({
            name: item.name,
            proxyUrl: item.proxyUrl,
            autoLocation: false,
            location: null,
          })), { silent: true, retry: false })
          for (const [index, item] of group.entries()) {
            const result = results[index]
            if (result?.status === 'succeeded') {
              item.record = result.data.record
              item.endpoint = result.data.record.endpoint
              item.status = 'succeeded'
              item.message = t('proxies.batchTest.addedStatus')
              item.input = ''
              item.proxyUrl = ''
            }
            else {
              const failure = proxyBatchFailure(result)
              item.status = failure.status
              item.message = failure.status === 'conflict'
                ? t('proxies.batchCreate.conflict')
                : failure.message
              if (failure.stop)
                stop()
            }
          }
        }
        catch (error) {
          const failure = proxyRequestFailure(error)
          for (const item of group) {
            item.status = failure.status
            item.message = failure.message
          }
          if (failure.stop)
            stop()
        }
      }
      if (testAfterCreate && !stopRequested.value && !disposed && created.value.length)
        await tests.run(created.value)
    }
    finally {
      for (const item of items.value) {
        if (item.status === 'pending') {
          item.status = 'skipped'
          item.message = t('proxies.batchCreate.stopped')
        }
      }
      busy.value = false
      if (!disposed)
        await options.reload()
    }
  }

  async function testCreated() {
    if (busy.value || disposed || !created.value.length)
      return
    busy.value = true
    stopRequested.value = false
    testingRequested.value = true
    try {
      await tests.run(created.value)
    }
    finally {
      busy.value = false
      if (!disposed)
        await options.reload()
    }
  }

  onScopeDispose(() => {
    disposed = true
    stop()
    text.value = ''
    for (const item of items.value) {
      item.input = ''
      item.proxyUrl = ''
    }
  })

  return {
    text,
    prefix,
    protocol,
    items,
    busy,
    stopping,
    testingRequested,
    tests,
    parsed,
    inputError,
    created,
    canEditFailures,
    submit,
    testCreated,
    stop,
    clear,
    editFailures,
  }
}
