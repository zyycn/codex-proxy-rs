import type { AccountProfileDailyUsage } from '@/api'
import { formatCompactNumber } from '@/utils/number'

export type ProfileActivityMode = 'daily' | 'weekly' | 'cumulative'
export type ProfileActivityLevel = 0 | 1 | 2 | 3 | 4

export interface ProfileActivityCell {
  date: string
  tokens: number
  value: number
  level: ProfileActivityLevel
  isFuture: boolean
}

export interface ProfileActivityWeek {
  key: string
  monthLabel: string | null
  cells: ProfileActivityCell[]
}

export interface ProfileActivityGrid {
  weeks: ProfileActivityWeek[]
  rangeStart: string
  rangeEnd: string
  totalTokens: number
}

const DAY_MS = 24 * 60 * 60 * 1000
const WEEK_COUNT = 52

export function buildProfileActivityGrid(
  entries: AccountProfileDailyUsage[],
  mode: ProfileActivityMode,
  today = new Date(),
): ProfileActivityGrid {
  const todayDate = localCalendarDate(today)
  const currentWeekStart = addDays(todayDate, -todayDate.getUTCDay())
  const rangeStartDate = addDays(currentWeekStart, -(WEEK_COUNT - 1) * 7)
  const dailyTokens = normalizedDailyTokens(entries, rangeStartDate, todayDate)
  const weeks: ProfileActivityWeek[] = []
  let cumulativeTokens = 0
  let totalTokens = 0

  for (let weekIndex = 0; weekIndex < WEEK_COUNT; weekIndex += 1) {
    const weekStart = addDays(rangeStartDate, weekIndex * 7)
    const weekDates = Array.from({ length: 7 }, (_, dayIndex) => addDays(weekStart, dayIndex))
    const weeklyTokens = weekDates.reduce((sum, date) => sum + (dailyTokens.get(dateKey(date)) ?? 0), 0)
    totalTokens += weeklyTokens
    const cells = weekDates.map((date) => {
      const key = dateKey(date)
      const tokens = dailyTokens.get(key) ?? 0
      const isFuture = date.getTime() > todayDate.getTime()
      cumulativeTokens += isFuture ? 0 : tokens
      return {
        date: key,
        tokens,
        value: mode === 'weekly' ? weeklyTokens : mode === 'cumulative' ? cumulativeTokens : tokens,
        level: 0 as ProfileActivityLevel,
        isFuture,
      }
    })
    weeks.push({
      key: dateKey(weekStart),
      monthLabel: monthLabel(weekDates, weekIndex),
      cells,
    })
  }

  const maxValue = Math.max(0, ...weeks.flatMap(week => week.cells)
    .filter(cell => !cell.isFuture)
    .map(cell => cell.value))
  for (const cell of weeks.flatMap(week => week.cells))
    cell.level = activityLevel(cell.value, maxValue)

  return {
    weeks,
    rangeStart: dateKey(rangeStartDate),
    rangeEnd: dateKey(todayDate),
    totalTokens,
  }
}

export function profileActivityCellLabel(cell: ProfileActivityCell, mode: ProfileActivityMode) {
  const date = new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    timeZone: 'UTC',
  }).format(parseDateKey(cell.date) ?? new Date(0))
  const value = formatCompactNumber(cell.value)
  if (mode === 'weekly')
    return `${date}所在周：${value} Tokens`
  if (mode === 'cumulative')
    return `截至${date}：${value} Tokens`
  return `${date}：${value} Tokens`
}

function normalizedDailyTokens(
  entries: AccountProfileDailyUsage[],
  rangeStart: Date,
  today: Date,
) {
  const result = new Map<string, number>()
  for (const entry of entries) {
    const date = parseDateKey(entry.date)
    if (!date || date < rangeStart || date > today || !Number.isFinite(entry.tokens))
      continue
    const key = dateKey(date)
    result.set(key, (result.get(key) ?? 0) + Math.max(0, entry.tokens))
  }
  return result
}

function activityLevel(value: number, maxValue: number): ProfileActivityLevel {
  if (value <= 0 || maxValue <= 0)
    return 0
  return Math.min(4, Math.max(1, Math.ceil(value / maxValue * 4))) as ProfileActivityLevel
}

function monthLabel(weekDates: Date[], weekIndex: number) {
  const firstDayOfMonth = weekDates.find(date => date.getUTCDate() === 1)
  if (weekIndex !== 0 && !firstDayOfMonth)
    return null
  const date = firstDayOfMonth ?? weekDates[0]
  return `${(date?.getUTCMonth() ?? 0) + 1}月`
}

function localCalendarDate(value: Date) {
  return new Date(Date.UTC(value.getFullYear(), value.getMonth(), value.getDate()))
}

function parseDateKey(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value)
  if (!match)
    return null
  const year = Number(match[1])
  const month = Number(match[2])
  const day = Number(match[3])
  const date = new Date(Date.UTC(year, month - 1, day))
  if (
    date.getUTCFullYear() !== year
    || date.getUTCMonth() !== month - 1
    || date.getUTCDate() !== day
  ) {
    return null
  }
  return date
}

function addDays(value: Date, days: number) {
  return new Date(value.getTime() + days * DAY_MS)
}

function dateKey(value: Date) {
  return value.toISOString().slice(0, 10)
}
