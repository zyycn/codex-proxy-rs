import type { ConfigType } from 'dayjs'
import dayjs from 'dayjs'

const DATE_TIME_FORMAT = 'YYYY-MM-DD HH:mm:ss'
const TIME_FORMAT = 'HH:mm:ss'

export function formatDateTime(value: ConfigType = new Date()): string {
  return dayjs(value).format(DATE_TIME_FORMAT)
}

export function formatTime(value: ConfigType = new Date()): string {
  return dayjs(value).format(TIME_FORMAT)
}

export function formatRelativeTime(
  value: ConfigType,
  now: ConfigType = new Date(),
): string {
  const timestamp = dayjs(value)
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
