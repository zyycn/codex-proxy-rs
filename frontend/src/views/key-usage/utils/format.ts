import { formatDateTime } from '@/utils/date'

export const KEY_USAGE_TIME_ZONE = 'Asia/Shanghai'

export function keyUsageTime(value: string | null) {
  return value ? formatDateTime(value, '—', KEY_USAGE_TIME_ZONE) : '—'
}

export function money(value: string | number | null, currency = 'USD') {
  if (value === null)
    return '未定价'
  const amount = Number(value)
  if (!Number.isFinite(amount))
    return '—'
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency,
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  }).format(amount)
}

export function freshInput(input: number, cached: number, written: number) {
  // 输入总量已经包含缓存读写，不把缓存再次计入消耗。
  return Math.max(0, input - cached - written)
}
