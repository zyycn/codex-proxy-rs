import type { AccountUsageStatisticsCost } from '@/api'

export function formatUsagePercent(value: number | null, ratio = false) {
  if (value === null)
    return '—'
  const percent = ratio ? value * 100 : value
  return `${percent >= 10 ? percent.toFixed(1) : percent.toFixed(2)}%`
}

export function formatUsageCost(
  cost: AccountUsageStatisticsCost | null,
  hasUnknownPricing = false,
) {
  if (!cost)
    return hasUnknownPricing ? '未公开' : '—'
  const amount = Number(cost.amount)
  const display = cost.currency === 'USD' && Number.isFinite(amount)
    ? `$${amount.toFixed(2)}`
    : `${cost.currency} ${cost.amount}`
  return `${display}${hasUnknownPricing ? '*' : ''}`
}
