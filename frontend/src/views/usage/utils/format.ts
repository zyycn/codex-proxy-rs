const compactNumberFormatter = new Intl.NumberFormat('zh-CN', {
  notation: 'compact',
  maximumFractionDigits: 1,
})

const numberFormatter = new Intl.NumberFormat('zh-CN', {
  maximumFractionDigits: 1,
})

const percentFormatter = new Intl.NumberFormat('zh-CN', {
  style: 'percent',
  maximumFractionDigits: 1,
})

export function formatCompactNumber(value: number) {
  return compactNumberFormatter.format(finiteOrZero(value))
}

export function formatNumber(value: number) {
  return numberFormatter.format(finiteOrZero(value))
}

export function formatPercent(value?: number | null) {
  return value == null || !Number.isFinite(value) ? '—' : percentFormatter.format(value)
}

export function formatSignedPercent(value?: number | null) {
  if (value == null || !Number.isFinite(value)) return '—'
  if (value === 0) return '0%'
  return `${value > 0 ? '+' : ''}${percentFormatter.format(value)}`
}

export function formatDuration(value?: number | null) {
  if (value == null || !Number.isFinite(value) || value < 0) return '—'
  if (value < 1_000) return `${Math.round(value)} ms`
  if (value < 60_000) {
    const seconds = value / 1_000
    return `${seconds.toFixed(seconds >= 10 ? 1 : 2).replace(/\.0+$|(?<=\.[0-9])0$/, '')} s`
  }
  return `${(value / 60_000).toFixed(1).replace(/\.0$/, '')} min`
}

export function formatDurationAxis(value: number) {
  if (!Number.isFinite(value)) return '—'
  if (value < 1_000) return `${Math.round(value)}ms`
  return `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}s`
}

export function formatUsd(value: number, precise = false) {
  const safeValue = finiteOrZero(value)
  const fractionDigits = precise || (Math.abs(safeValue) > 0 && Math.abs(safeValue) < 0.01) ? 4 : 2
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: fractionDigits,
  }).format(safeValue)
}

export function formatUsdAxis(value: number) {
  const safeValue = finiteOrZero(value)
  if (Math.abs(safeValue) >= 1_000) return `$${formatCompactNumber(safeValue)}`
  if (Math.abs(safeValue) < 0.01 && safeValue !== 0) return `$${safeValue.toFixed(3)}`
  return `$${safeValue.toFixed(safeValue < 1 ? 2 : 1)}`
}

export function escapeTooltip(value: string) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function finiteOrZero(value: number) {
  return Number.isFinite(value) ? value : 0
}
