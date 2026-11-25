import type { KeyUsageOverview, KeyUsageRecordKind } from '@/api'
import { refDebounced, useDocumentVisibility, useTimeoutPoll } from '@vueuse/core'
import { computed, shallowRef, watch } from 'vue'
import { getKeyUsageOverview, getKeyUsageRecords } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'

export function useKeyUsage() {
  const period = shallowRef<'today' | '7d' | '30d'>('today')
  const model = shallowRef('')
  const selectedModel = refDebounced(model, 300)
  const kind = shallowRef<KeyUsageRecordKind>('success')
  const refreshInterval = shallowRef('30')
  const overview = shallowRef<KeyUsageOverview>()
  const refreshing = shallowRef(false)
  const overviewRequest = useRequestState()
  const rangeEnd = shallowRef(Date.now())
  const query = computed(() => {
    return {
      period: period.value,
      asOf: rangeEnd.value,
      model: selectedModel.value.trim() || undefined,
    }
  })
  const records = useStablePagedQuery({
    initialPageSize: 20,
    load: (pagination, options) => getKeyUsageRecords({
      ...query.value,
      ...pagination,
      kind: kind.value,
    }, options),
    onSuccess: () => {
      records.error.value = ''
    },
  })

  async function loadOverview(silent = false) {
    const id = overviewRequest.start()
    try {
      const result = await getKeyUsageOverview(query.value, { signal: overviewRequest.signal, silent })
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

  async function refresh(silent = false) {
    if (refreshing.value || overviewRequest.loading.value || records.loading.value)
      return
    refreshing.value = true
    rangeEnd.value = Date.now()
    try {
      await Promise.all([
        loadOverview(silent),
        records.execute(undefined, { silent }),
      ])
    }
    finally {
      refreshing.value = false
    }
  }

  watch([period, selectedModel], () => {
    rangeEnd.value = Date.now()
    overview.value = undefined
    void loadOverview()
    void records.reloadFromStart()
  }, { immediate: true })

  watch(kind, () => {
    void records.reloadFromStart()
  })

  const visibility = useDocumentVisibility()
  const poll = useTimeoutPoll(() => refresh(true), computed(() => Math.max(1, Number(refreshInterval.value)) * 1000))
  watch([visibility, refreshInterval], ([visible, interval]) => {
    if (visible === 'visible' && interval !== '0')
      poll.resume()
    else
      poll.pause()
  }, { immediate: true })

  function changePage(page: number) {
    void records.execute(page)
  }

  function changePageSize(size: number) {
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
    overviewLoading: overviewRequest.loading,
    records,
    refresh,
    changePageSize,
    changePage,
  }
}
