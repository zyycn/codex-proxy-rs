import type { LineSeriesOption } from 'echarts'
import type { useChartPalette } from '@/composables/useChartPalette'
import { sampleGapBridgeSeries } from '@/components/charts/timeSeriesGap'
import { chartTooltipStyle } from '@/components/charts/tooltip'

import { formatLocalizedCompactNumber } from '@/utils/format'

type UsageChartPalette = ReturnType<typeof useChartPalette>['palette']['value']
type UsageAreaStrength = 'strong' | 'subtle'

interface UsageLineSeriesOptions {
  stack?: string
  area?: UsageAreaStrength | false
  xAxisIndex?: number
  yAxisIndex?: number
}

const areaAlpha: Record<UsageAreaStrength, readonly [string, string]> = {
  strong: ['30', '08'],
  subtle: ['18', '02'],
}

function usageLineSeries(
  name: string,
  data: Array<number | null | undefined>,
  color: string,
  options: UsageLineSeriesOptions = {},
): LineSeriesOption {
  const area = options.area ?? (options.stack ? 'strong' : false)
  const alpha = area ? areaAlpha[area] : null
  const sampleCount = data.reduce<number>(
    (count, value) => count + (value == null
      ? 0
      : 1),
    0,
  )
  const showSymbols = sampleCount > 0 && sampleCount <= 12

  return {
    name,
    type: 'line',
    data: data.map(value => value ?? null),
    connectNulls: false,
    stack: options.stack,
    xAxisIndex: options.xAxisIndex ?? 0,
    yAxisIndex: options.yAxisIndex ?? 0,
    smooth: true,
    showSymbol: showSymbols,
    showAllSymbol: showSymbols,
    symbol: 'circle',
    symbolSize: sampleCount === 1
      ? 7
      : 5,
    lineStyle: { color, width: 2.2 },
    itemStyle: { color },
    areaStyle: alpha
      ? {
          color: {
            type: 'linear',
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: `${color}${alpha[0]}` },
              { offset: 1, color: `${color}${alpha[1]}` },
            ],
          },
        }
      : undefined,
  }
}

export function usageGapAwareLineSeries(
  name: string,
  data: Array<number | null | undefined>,
  color: string,
  options: UsageLineSeriesOptions = {},
) {
  const primary = usageLineSeries(name, data, color, options)
  return [
    primary,
    ...sampleGapBridgeSeries(data, {
      name,
      color,
      xAxisIndex: options.xAxisIndex,
      yAxisIndex: options.yAxisIndex,
      width: 2.2,
      z: 2,
    }),
  ]
}

export function usageTooltip(
  theme: UsageChartPalette,
  formatter: (params: unknown) => string,
) {
  return {
    trigger: 'axis' as const,
    ...chartTooltipStyle(theme, { axisPointer: true, fontWeight: 600 }),
    formatter,
  }
}

export function usageTooltipItem(label: string, value: string, color: string) {
  return `<span style="display:inline-block;width:7px;height:7px;margin-right:6px;border-radius:999px;background:${escapeTooltip(color)}"></span>${escapeTooltip(label)}: ${escapeTooltip(value)}`
}

export function usageCategoryAxis(labels: string[], theme: UsageChartPalette) {
  return {
    type: 'category' as const,
    data: labels,
    axisLabel: {
      color: theme.textMuted,
      fontSize: 10,
      fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim() || 'monospace',
      hideOverlap: true,
    },
    axisLine: { show: false },
    axisTick: { show: false },
  }
}

export function usageValueAxis(
  theme: UsageChartPalette,
  formatter: (value: number) => string,
  options: { min?: number, max?: number, splitLine?: boolean } = {},
) {
  return {
    type: 'value' as const,
    min: options.min,
    max: options.max,
    splitNumber: 3,
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: {
      show: true,
      color: theme.textMuted,
      fontSize: 10,
      fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim() || 'monospace',
      formatter,
    },
    splitLine: {
      show: options.splitLine !== false,
      lineStyle: { color: theme.grid, width: 1 },
    },
  }
}

export function usageLegend(theme: UsageChartPalette, data: string[]) {
  return {
    top: 0,
    right: 4,
    itemWidth: 8,
    itemHeight: 8,
    icon: 'circle' as const,
    data,
    textStyle: {
      color: theme.textSecondary,
      fontSize: 11,
      fontFamily: getComputedStyle(document.documentElement).fontFamily,
      fontWeight: 650,
    },
  }
}

export function formatDurationAxis(value: number) {
  if (!Number.isFinite(value))
    return '—'
  if (value < 1_000)
    return `${Math.round(value)} ms`
  return `${(value / 1_000).toFixed(value >= 10_000
    ? 0
    : 1)} s`
}

export function formatUsdAxis(value: number) {
  const safeValue = Number.isFinite(value)
    ? value
    : 0
  if (Math.abs(safeValue) >= 1_000)
    return `$${formatLocalizedCompactNumber(safeValue)}`
  if (Math.abs(safeValue) < 0.01 && safeValue !== 0)
    return `$${safeValue.toFixed(3)}`
  return `$${safeValue.toFixed(safeValue < 1
    ? 2
    : 1)}`
}

function escapeTooltip(value: string) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll('\'', '&#39;')
}
