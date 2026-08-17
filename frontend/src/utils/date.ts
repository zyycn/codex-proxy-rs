import type { ConfigType } from 'dayjs'
import dayjs from 'dayjs'

const DATE_TIME_FORMAT = 'YYYY-MM-DD HH:mm:ss'
const TIME_FORMAT = 'HH:mm:ss'

export function formatDateTime(value: ConfigType = new Date(), fallback = '—'): string {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.format(DATE_TIME_FORMAT) : fallback
}

export function formatTime(value: ConfigType = new Date(), fallback = '—'): string {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.format(TIME_FORMAT) : fallback
}

export function parseTimestamp(value: ConfigType): number | null {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.valueOf() : null
}

export function formatRelativeTime(
  value: ConfigType,
  now: ConfigType = new Date(),
): string {
  const timestamp = normalizedDate(value)
  if (!timestamp.isValid())
    return '—'

  const elapsedSeconds = Math.max(0, dayjs(now).diff(timestamp, 'second'))
  if (elapsedSeconds < 60)
    return '刚刚'

  const elapsedMinutes = Math.floor(elapsedSeconds / 60)
  if (elapsedMinutes < 60)
    return `${elapsedMinutes} 分钟前`

  const elapsedHours = Math.floor(elapsedMinutes / 60)
  if (elapsedHours < 24)
    return `${elapsedHours} 小时前`

  return `${Math.floor(elapsedHours / 24)} 天前`
}

function normalizedDate(value: ConfigType) {
  return dayjs(typeof value === 'string' ? value.replace(' ', 'T') : value)
}
