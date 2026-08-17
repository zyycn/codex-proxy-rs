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
