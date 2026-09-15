import type { KeyUsageOverview, KeyUsageRecordKind } from '@/api/modules/key-usage'
import { refDebounced, useDocumentVisibility, useTimeoutPoll } from '@vueuse/core'
import dayjs from 'dayjs'
import { computed, shallowRef, watch } from 'vue'
import { getKeyUsageOverview, getKeyUsageRecords } from '@/api/modules/key-usage'
import { useRequestState } from '@/composables/useRequestState'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { KEY_USAGE_TIME_ZONE } from '../utils/format'

export function useKeyUsage() {
  const period = shallowRef('today')
  const model = shallowRef('')
  const selectedModel = refDebounced(model, 300)
  const kind = shallowRef<KeyUsageRecordKind>('success')
  const refreshInterval = shallowRef('30')
  const overview = shallowRef<KeyUsageOverview>()
  const refreshing = shallowRef(false)
  const recordsStale = shallowRef(false)
  const overviewRequest = useRequestState()
  let queryGeneration = 0
  const rangeEnd = shallowRef(dayjs())
  const query = computed(() => {
    const days = period.value === '7d' ? 6 : period.value === '30d' ? 29 : 0
    return {
      startTime: rangeEnd.value.tz(KEY_USAGE_TIME_ZONE).subtract(days, 'day').startOf('day').toISOString(),
      endTime: rangeEnd.value.toISOString(),
      model: selectedModel.value.trim() || undefined,
    }
  })
  const records = useStablePagedQuery({
    initialPageSize: 20,
    load: (pagination, options) => getKeyUsageRecords({ ...query.value, ...pagination, kind: kind.value }, { ...options, silent: true }),
    onSuccess: () => {
      recordsStale.value = false
      records.error.value = ''
    },
  })

  async function loadOverview() {
    const id = overviewRequest.start()
    try {
      const result = await getKeyUsageOverview(query.value, { signal: overviewRequest.signal, silent: true })
      if (overviewRequest.isCurrent(id))
        overview.value = result
    }
    catch (cause) {
      overviewRequest.fail(id, cause)
    }
    finally {
      overviewRequest.finish(id)
    }
  }

  async function refresh() {
    if (refreshing.value || overviewRequest.loading.value || records.loading.value)
      return
    refreshing.value = true
    const generation = queryGeneration
    rangeEnd.value = dayjs()
    try {
      const [, recordsOk] = await Promise.all([loadOverview(), records.execute(undefined, { silent: true })])
      // 轮询被新筛选或翻页取代时，不把取消结果标成刷新失败。
      if (generation === queryGeneration)
        recordsStale.value = !recordsOk
    }
    finally {
      refreshing.value = false
    }
  }

  watch([period, selectedModel], () => {
    queryGeneration += 1
    rangeEnd.value = dayjs()
    overview.value = undefined
    records.items.value = []
    void loadOverview()
    void records.reloadFromStart()
  }, { immediate: true })

  watch(kind, () => {
    queryGeneration += 1
    records.items.value = []
    void records.reloadFromStart()
  })

  const visibility = useDocumentVisibility()
  const poll = useTimeoutPoll(refresh, computed(() => Math.max(1, Number(refreshInterval.value)) * 1000))
  watch([visibility, refreshInterval], ([visible, interval]) => {
    if (visible === 'visible' && interval !== '0')
      poll.resume()
    else
      poll.pause()
  }, { immediate: true })

  function changePage(page: number) {
    queryGeneration += 1
    void records.execute(page)
  }

  function changePageSize(size: number) {
    queryGeneration += 1
    records.pageSize.value = size
    void records.reloadFromStart()
  }

  return {
    period,
    model,
    kind,
    refreshInterval,
    overview,
    refreshing,
    recordsStale,
    overviewLoading: overviewRequest.loading,
    overviewError: overviewRequest.error,
    records,
    refresh,
    changePageSize,
    changePage,
  }
}
