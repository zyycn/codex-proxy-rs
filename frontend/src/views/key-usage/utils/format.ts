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
