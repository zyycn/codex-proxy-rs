import type { AccountQuotaWindow } from '@/api/index'
import { clamp } from 'es-toolkit'
import { t } from '@/i18n'
import { isRecord } from '@/utils/data'
import { formatInteger } from '@/utils/format'

export type AccountUsageWindowVariant = 'compact' | 'detail' | 'metric'

type AccountUsageWindowMode = 'quota' | 'local' | 'unknown'

interface AccountRequestBucket {
  bucketStart: string
  label: string
  requestCount: number
}

export interface AccountRequestBar {
  key: string
  requestCount: number
  height: string
  title: string
}

interface AccountLocalUsage {
  requestCount?: number
  requestCountDisplay?: string
  totalTokens?: number
  totalTokensDisplay?: string
  requestBuckets?: AccountRequestBucket[]
}

interface AccountUsageWindowVariantDefinition {
  rootClass: string
  headerClass: string
  labelClass: string
  valueClass: string
  trackOffsetClass: string
  trackShapeClass: string
  minimumBarWidth: string
  compactValues: boolean
  labelTooltip: boolean
  showResetTime: boolean
}

type QuotaWindowTone = 'danger' | 'success' | 'unknown' | 'warning'

interface AccountUsageWindowPresentationInput {
  window: AccountQuotaWindow | undefined
  variant: AccountUsageWindowVariant
  showLocalValue: boolean
}

const variantDefinitions: Record<
  AccountUsageWindowVariant,
  AccountUsageWindowVariantDefinition
> = {
  metric: {
    rootClass: 'grid min-w-0 grid-rows-[14px_14px] gap-1',
    headerClass: 'flex items-center justify-between gap-2 text-cp-xs leading-[14px] font-bold',
    labelClass: 'truncate text-cp-text-quaternary',
    valueClass: 'text-cp-xs leading-none font-bold',
    trackOffsetClass: 'self-center',
    trackShapeClass: 'h-1.5 w-full overflow-hidden rounded-full',
    minimumBarWidth: '6px',
    compactValues: false,
    labelTooltip: true,
    showResetTime: false,
  },
  compact: {
    rootClass: 'flex min-w-0 flex-col',
    headerClass: 'mb-1 flex items-baseline justify-between gap-2 text-cp-xs leading-[14px] font-bold',
    labelClass: 'truncate text-cp-text-quaternary',
    valueClass: 'text-[10px] leading-[14px] font-heavy',
    trackOffsetClass: 'mt-auto',
    trackShapeClass: 'h-[3px] w-full overflow-hidden rounded-full',
    minimumBarWidth: '3px',
    compactValues: true,
    labelTooltip: true,
    showResetTime: false,
  },
  detail: {
    rootClass: 'rounded-lg bg-cp-fill-quaternary p-2',
    headerClass: 'flex items-center justify-between gap-3 text-cp-sm font-bold',
    labelClass: 'text-cp-text-secondary',
    valueClass: 'text-cp-sm font-heavy',
    trackOffsetClass: 'mt-2',
    trackShapeClass: 'h-2 overflow-hidden rounded-full',
    minimumBarWidth: '8px',
    compactValues: false,
    labelTooltip: false,
    showResetTime: true,
  },
}

const quotaToneClasses: Record<
  QuotaWindowTone,
  { bar: string, text: string }
> = {
  unknown: {
    bar: 'bg-cp-border',
    text: 'text-cp-text-quaternary',
  },
  danger: {
    bar: 'bg-cp-error',
    text: 'text-cp-error-text',
  },
  warning: {
    bar: 'bg-cp-warning',
    text: 'text-cp-warning-text',
  },
  success: {
    bar: 'bg-cp-success',
    text: 'text-cp-success-text',
  },
}

export function resolveAccountUsageWindowPresentation(
  input: AccountUsageWindowPresentationInput,
) {
  const definition = variantDefinitions[input.variant]
  const localUsage = accountLocalUsage(input.window?.localUsage)
  const mode = usageWindowMode(input.window, localUsage)
  const quotaLocalUsageDisplay = localTokenDisplay(localUsage)
  const quotaLocalUsageVisible = input.showLocalValue && Boolean(quotaLocalUsageDisplay)
  const localRequestDisplay = requestCountDisplay(localUsage)
  const localRequestValueVisible = input.showLocalValue
    && typeof localUsage?.requestCount === 'number'
    && localUsage.requestCount > 0
  const localRequestLabel = t('accounts.quota.dailyRequests')
  const quota = input.window
    ? quotaWindowPresentation(input.window, definition.minimumBarWidth)
    : null
  const label = input.window
    ? quotaWindowLabel(input.window)
    : undefined

  return {
    mode,
    compactValues: definition.compactValues,
    label,
    labelTooltip: definition.labelTooltip ? label : undefined,
    classes: {
      root: definition.rootClass,
      header: definition.headerClass,
      label: definition.labelClass,
      value: definition.valueClass,
      trackOffset: definition.trackOffsetClass,
      trackShape: definition.trackShapeClass,
      track: `${definition.trackShapeClass} bg-cp-border-secondary`,
    },
    quota: {
      valueVisible: typeof input.window?.usedPercent === 'number'
        && (input.variant !== 'compact' || input.window.usedPercent > 0),
      localUsageDisplay: quotaLocalUsageDisplay,
      localUsageVisible: quotaLocalUsageVisible,
      barStyle: quota?.barStyle,
      barClass: quota?.barClass,
      percentTextClass: quota?.percentTextClass,
      resetVisible: Boolean(input.window)
        && definition.showResetTime
        && input.window?.resetAtDisplay !== '—',
    },
    local: {
      label: localRequestLabel,
      requestDisplay: localRequestDisplay,
      requestValueVisible: localRequestValueVisible,
      timelineTitle: t('accounts.quota.requests', { localRequestLabel, localRequestDisplay }),
      requestBars: requestTimeline(localUsage?.requestBuckets ?? []),
      durationDisplay: rollingWindowDurationDisplay(input.window?.windowSeconds),
    },
  }
}

function accountLocalUsage(value: unknown): AccountLocalUsage | null {
  if (!isRecord(value))
    return null

  const localUsage: AccountLocalUsage = {
    requestBuckets: requestBuckets(value.requestBuckets),
  }
  if (finiteNumber(value.requestCount))
    localUsage.requestCount = value.requestCount
  if (typeof value.requestCountDisplay === 'string')
    localUsage.requestCountDisplay = value.requestCountDisplay
  if (finiteNumber(value.totalTokens))
    localUsage.totalTokens = value.totalTokens
  if (typeof value.totalTokensDisplay === 'string')
    localUsage.totalTokensDisplay = value.totalTokensDisplay
  return localUsage
}

function finiteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

function requestBuckets(value: unknown) {
  if (!Array.isArray(value))
    return []

  return value.flatMap((item) => {
    if (!isRecord(item))
      return []
    if (
      typeof item.bucketStart !== 'string'
      || !finiteNumber(item.requestCount)
      || typeof item.label !== 'string'
    ) {
      return []
    }
    return [
      {
        bucketStart: item.bucketStart,
        label: item.label,
        requestCount: item.requestCount,
      },
    ]
  })
}

function usageWindowMode(
  window: AccountQuotaWindow | undefined,
  localUsage: AccountLocalUsage | null,
): AccountUsageWindowMode {
  if (typeof window?.usedPercent === 'number')
    return 'quota'
  if (localUsage)
    return 'local'
  return window ? 'quota' : 'unknown'
}

function localTokenDisplay(localUsage: AccountLocalUsage | null) {
  const total = localUsage?.totalTokens
  if (typeof total !== 'number' || total <= 0)
    return ''

  const display = localUsage?.totalTokensDisplay
  if (typeof display === 'string' && display.trim())
    return display.trim()
  return formatInteger(total)
}

export function quotaWindowDetails(window: AccountQuotaWindow) {
  const localUsage = accountLocalUsage(window.localUsage)
  const tokenDisplay = localTokenDisplay(localUsage) || null
  const amountDisplay = window.amount?.usedDisplay ?? null
  const amount = amountDisplay?.replace(/[^\d.]/g, '')
  const amountUsedDisplay = amount && Number(amount) === 0
    ? null
    : amountDisplay
  const hasConsumption = Boolean(tokenDisplay || amountUsedDisplay)
    || (!window.amount && localUsage?.totalTokens == null)
  return {
    localUsageDisplay: tokenDisplay,
    amountUsedDisplay,
    consumptionDisplay: hasConsumption
      ? [
          amountDisplay,
          tokenDisplay || (localUsage?.totalTokens === 0 || window.amount
            ? '0'
            : '—'),
        ].filter(Boolean).join(' · ')
      : null,
    resetAtDisplay: window.amount && window.resetAtDisplay === '—'
      ? t('accounts.upstream.startsOnNextUse')
      : window.resetAtDisplay,
  }
}

const quotaWindowPeriods = [
  {
    seconds: 18_000,
    label: () => t('accounts.quota.windows.fiveHour'),
  },
  {
    seconds: 86_400,
    label: () => t('accounts.quota.windows.daily'),
  },
  {
    seconds: 604_800,
    label: () => t('accounts.quota.windows.weekly'),
  },
  {
    seconds: 2_592_000,
    label: () => t('accounts.quota.windows.monthly'),
  },
  {
    seconds: 31_536_000,
    label: () => t('accounts.quota.windows.annual'),
  },
]

export function quotaWindowLabel(window: AccountQuotaWindow, includeLimitName = true) {
  const seconds = window.windowSeconds
  // 沿用 Codex 官方的显示时长容差；仅决定文案，不改变后端窗口归属或额度判定。
  const period = seconds === null
    ? undefined
    : quotaWindowPeriods.find(period =>
        seconds >= period.seconds * 0.95 && seconds <= period.seconds * 1.05,
      )
  let label: string
  if (period) {
    label = period.label()
  }
  else if (seconds !== null && seconds > 0) {
    if (seconds % 86_400 === 0) {
      label = t('accounts.quota.windows.days', {
        duration: formatInteger(seconds / 86_400),
      })
    }
    else if (seconds % 3_600 === 0) {
      label = t('accounts.quota.windows.hours', {
        duration: formatInteger(seconds / 3_600),
      })
    }
    else {
      label = t('accounts.quota.windows.minutes', {
        duration: formatInteger(Math.ceil(seconds / 60)),
      })
    }
  }
  else if (window.role === 'monthly') {
    label = t('accounts.quota.windows.monthly')
  }
  else if (window.role === 'primary') {
    label = t('accounts.quota.windows.primary')
  }
  else if (window.role === 'secondary') {
    label = t('accounts.quota.windows.secondary')
  }
  else {
    label = window.windowLabelDisplay
  }

  return includeLimitName && window.limitName && window.limitId !== 'codex'
    ? `${window.limitName} · ${label}`
    : label
}

function requestCountDisplay(localUsage: AccountLocalUsage | null) {
  const display = localUsage?.requestCountDisplay
  if (typeof display === 'string' && display.trim())
    return display.trim()
  const count = localUsage?.requestCount
  return typeof count === 'number'
    ? formatInteger(count)
    : '0'
}

function rollingWindowDurationDisplay(windowSeconds: number | null | undefined) {
  if (typeof windowSeconds !== 'number' || !Number.isFinite(windowSeconds) || windowSeconds <= 0)
    return '—'

  const hourSeconds = 60 * 60
  const daySeconds = 24 * hourSeconds
  if (windowSeconds <= daySeconds * 2 && windowSeconds % hourSeconds === 0) {
    return t('accounts.quota.h', {
      duration: formatInteger(windowSeconds / hourSeconds),
    })
  }
  if (windowSeconds % daySeconds === 0) {
    return t('accounts.quota.d', {
      duration: formatInteger(windowSeconds / daySeconds),
    })
  }
  if (windowSeconds % hourSeconds === 0) {
    return t('accounts.quota.h', {
      duration: formatInteger(windowSeconds / hourSeconds),
    })
  }
  return t('accounts.quota.s', {
    duration: formatInteger(windowSeconds),
  })
}

export function quotaWindowPresentation(window: AccountQuotaWindow, minimumWidth: string) {
  const percent = clamp(window.usedPercent ?? 0, 0, 100)
  const tone = quotaWindowTone(window.usedPercent)
  return {
    barStyle: {
      width: `${percent}%`,
      minWidth: percent > 0
        ? minimumWidth
        : '0',
    },
    barClass: quotaToneClasses[tone].bar,
    percentTextClass: quotaToneClasses[tone].text,
  }
}

export function quotaWindowTone(usedPercent: number | null): QuotaWindowTone {
  if (usedPercent === null)
    return 'unknown'
  // 每个窗口按自身的已用比例分档，剩余不足 30% / 10% 时分别预警
  if (usedPercent > 90)
    return 'danger'
  if (usedPercent > 70)
    return 'warning'
  return 'success'
}

function requestTimeline(buckets: NonNullable<AccountLocalUsage['requestBuckets']>): AccountRequestBar[] {
  const maximum = Math.max(1, ...buckets.map(bucket => bucket.requestCount))
  return buckets.map(bucket => ({
    key: bucket.bucketStart,
    requestCount: bucket.requestCount,
    height: bucket.requestCount === 0
      ? '0'
      : `${Math.max(25, Math.round(bucket.requestCount / maximum * 100))}%`,
    title: bucket.label,
  }))
}
