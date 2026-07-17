import type { Dayjs } from 'dayjs'
import dayjs from 'dayjs'
import { computed, shallowRef } from 'vue'

export type UsageTimeRange = 'today' | '7d' | '30d'

export interface UsageTimeRangeParams extends Record<string, string> {
  startTime: string
  endTime: string
}

export function useUsageTimeRange(initialRange: UsageTimeRange = 'today') {
  const timeRange = shallowRef<UsageTimeRange>(initialRange)
  const rangeEnd = shallowRef(dayjs())
  const timeRangeParams = computed(() => buildTimeRange(timeRange.value, rangeEnd.value))

  function refreshTimeRangeEnd() {
    rangeEnd.value = dayjs()
  }

  function latestTimeRangeParams() {
    return buildTimeRange(timeRange.value, dayjs())
  }

  return {
    timeRange,
    timeRangeParams,
    refreshTimeRangeEnd,
    latestTimeRangeParams,
  }
}

function buildTimeRange(range: UsageTimeRange, end: Dayjs): UsageTimeRangeParams {
  if (range === 'today') {
    return {
      startTime: end.startOf('day').toISOString(),
      endTime: end.toISOString(),
    }
  }

  const days = range === '30d' ? 29 : 6
  return {
    startTime: end.subtract(days, 'day').startOf('day').toISOString(),
    endTime: end.toISOString(),
  }
}
