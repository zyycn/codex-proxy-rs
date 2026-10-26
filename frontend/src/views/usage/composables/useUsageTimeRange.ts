import type { DatePickerValue } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'

export interface UsageTimeRangeParams {
  startTime: string
  endTime: string
}

function dateStart(date: Date, offset = 0) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + offset)
}

export function useUsageTimeRange() {
  const now = new Date()
  const selectedDates = shallowRef<[Date, Date]>([
    dateStart(now),
    dateStart(now),
  ])
  const timeRange = computed<DatePickerValue>({
    get: () => selectedDates.value,
    set: (value) => {
      if (!Array.isArray(value) || value.length !== 2)
        return
      const [start, end] = value
      if (start instanceof Date && end instanceof Date && !disabledDate(start) && !disabledDate(end) && start <= end)
        selectedDates.value = [start, end]
    },
  })
  const rangeEnd = shallowRef(now.getTime())
  const timeRangeParams = computed(() => rangeParams(rangeEnd.value))

  function disabledDate(date: Date) {
    const today = dateStart(new Date())
    const value = dateStart(date).getTime()
    return !Number.isFinite(value) || value < dateStart(today, -30).getTime() || value > today.getTime()
  }

  function rangeParams(asOf: number): UsageTimeRangeParams {
    const [start, end] = selectedDates.value
    // 按日历推进到结束日期的次日零点，避免夏令时切换时漏计或多计一小时
    return {
      startTime: dateStart(start).toISOString(),
      endTime: new Date(Math.min(dateStart(end, 1).getTime(), asOf)).toISOString(),
    }
  }

  function refreshTimeRangeEnd() {
    rangeEnd.value = Date.now()
  }

  function latestTimeRangeParams() {
    return rangeParams(Date.now())
  }

  return {
    timeRange,
    timeRangeParams,
    disabledDate,
    refreshTimeRangeEnd,
    latestTimeRangeParams,
  }
}
