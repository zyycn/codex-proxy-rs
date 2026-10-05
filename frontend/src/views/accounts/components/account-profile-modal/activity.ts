import type { ProfileActivityCalendar } from '@/api'
import { formatCompactNumber } from '@/utils/format'

export type ProfileActivityMode = 'daily' | 'weekly' | 'cumulative'
export type ProfileActivityLevel = 0 | 1 | 2 | 3 | 4

export function buildProfileActivityGrid(calendar: ProfileActivityCalendar | null, mode: ProfileActivityMode) {
  let cumulative = 0
  const weeks = (calendar?.weeks ?? []).map((week) => {
    const weekly = week.cells.reduce((sum, cell) => sum + cell.tokens, 0)
    return { ...week, cells: week.cells.map((cell) => {
      cumulative += cell.isFuture ? 0 : cell.tokens
      return { ...cell, value: mode === 'weekly' ? weekly : mode === 'cumulative' ? cumulative : cell.tokens, level: 0 as ProfileActivityLevel }
    }) }
  })
  const maximum = Math.max(0, ...weeks.flatMap(week => week.cells).filter(cell => !cell.isFuture).map(cell => cell.value))
  for (const cell of weeks.flatMap(week => week.cells))
    cell.level = cell.value <= 0 || maximum <= 0 ? 0 : Math.min(4, Math.max(1, Math.ceil(cell.value / maximum * 4))) as ProfileActivityLevel
  return { weeks, rangeLabel: calendar?.rangeLabel ?? '' }
}

export function profileActivityCellLabel(cell: { dateDisplay: string, value: number }, mode: ProfileActivityMode) {
  const value = formatCompactNumber(cell.value)
  if (mode === 'weekly')
    return `${cell.dateDisplay}所在周：${value} Tokens`
  if (mode === 'cumulative')
    return `截至${cell.dateDisplay}：${value} Tokens`
  return `${cell.dateDisplay}：${value} Tokens`
}
