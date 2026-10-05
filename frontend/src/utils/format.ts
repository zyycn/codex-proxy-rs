const integerFormatter = new Intl.NumberFormat('zh-CN')

const localizedCompactFormatter = new Intl.NumberFormat('zh-CN', {
  notation: 'compact',
  maximumFractionDigits: 1,
})

const metricUnits = [
  ['P', 1_000_000_000_000_000],
  ['T', 1_000_000_000_000],
  ['B', 1_000_000_000],
  ['M', 1_000_000],
  ['K', 1_000],
] as const

export function formatInteger(value: number) {
  return integerFormatter.format(value)
}

export function formatLocalizedCompactNumber(value: number) {
  return localizedCompactFormatter.format(Number.isFinite(value) ? value : 0)
}

export function formatCompactNumber(value: number) {
  const normalized = Math.max(0, Math.round(value))
  if (normalized < 1_000)
    return formatInteger(normalized)

  for (const [unit, threshold] of metricUnits) {
    if (normalized < threshold)
      continue

    const scaled = normalized / threshold
    const rounded = scaled >= 10 ? scaled.toFixed(1) : scaled.toFixed(2)
    return `${rounded.replace(/\.?0+$/, '')}${unit}`
  }

  return formatInteger(normalized)
}

const percentFormatter = new Intl.NumberFormat('zh-CN', {
  style: 'percent',
  maximumFractionDigits: 1,
})

export function formatPercent(value?: number | null) {
  return value == null || !Number.isFinite(value) ? '—' : percentFormatter.format(value)
}

export function formatDuration(value?: number | null) {
  if (value == null || !Number.isFinite(value) || value < 0)
    return '—'
  if (value < 1_000)
    return `${Math.round(value)} ms`
  if (value < 60_000) {
    const seconds = value / 1_000
    return `${seconds.toFixed(seconds >= 10 ? 1 : 2).replace(/\.0+$|(?<=\.\d)0$/, '')} s`
  }
  return `${(value / 60_000).toFixed(1).replace(/\.0$/, '')} min`
}

export function decimalDisplayNumber(value?: string | number | null) {
  if (value == null)
    return null
  const parsed = typeof value === 'number' ? value : Number(value)
  return Number.isFinite(parsed) ? parsed : null
}

export function formatUsd(value?: string | number | null, precise = false) {
  const parsed = decimalDisplayNumber(value)
  if (parsed == null)
    return '—'
  const fractionDigits = precise || (Math.abs(parsed) > 0 && Math.abs(parsed) < 0.01) ? 4 : 2
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: fractionDigits,
  }).format(parsed)
}
