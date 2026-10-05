import { computed, shallowRef } from 'vue'

type UsageTimeRange = 'today' | '7d' | '30d'
export interface UsageTimeRangeParams {
  period: UsageTimeRange
  asOf: number
}

export function useUsageTimeRange(initialRange: UsageTimeRange = 'today') {
  const timeRange = shallowRef<UsageTimeRange>(initialRange)
  const rangeEnd = shallowRef(Date.now())
  const timeRangeParams = computed(() => ({ period: timeRange.value, asOf: rangeEnd.value }))
  function refreshTimeRangeEnd() {
    rangeEnd.value = Date.now()
  }
  function latestTimeRangeParams() {
    return { period: timeRange.value, asOf: Date.now() }
  }
  return { timeRange, timeRangeParams, refreshTimeRangeEnd, latestTimeRangeParams }
}
