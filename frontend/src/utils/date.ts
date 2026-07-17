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
