export function formatDashboardRate(value: number) {
  return Number.isFinite(value)
    ? `${(value * 100).toFixed(1)}%`
    : '0%'
}
