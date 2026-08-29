import type { LineSeriesOption } from 'echarts'
import type { useChartPalette } from '@/composables/useChartPalette'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { escapeTooltip } from './format'

type UsageChartPalette = ReturnType<typeof useChartPalette>['palette']['value']
type UsageAreaStrength = 'strong' | 'subtle'

interface UsageLineSeriesOptions {
  stack?: string
  area?: UsageAreaStrength | false
}

const areaAlpha: Record<UsageAreaStrength, readonly [string, string]> = {
  strong: ['30', '08'],
  subtle: ['18', '02'],
}

export function usageLineSeries(
  name: string,
  data: Array<number | null | undefined>,
  color: string,
  options: UsageLineSeriesOptions = {},
): LineSeriesOption {
  const area = options.area ?? (options.stack ? 'strong' : false)
  const alpha = area ? areaAlpha[area] : null

  return {
    name,
    type: 'line',
    data: data.map(value => value ?? null),
    connectNulls: false,
    stack: options.stack,
    smooth: true,
    showSymbol: data.length <= 12,
    symbol: 'circle',
    symbolSize: 5,
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

export function usageTooltip(
  theme: UsageChartPalette,
  formatter: (params: unknown) => string,
) {
  return {
    trigger: 'axis' as const,
    ...chartTooltipStyle(theme, { axisPointer: true }),
    formatter,
  }
}

export function usageTooltipContent(
  theme: UsageChartPalette,
  label: string,
  lines: string[],
) {
  const title = escapeTooltip(label)
  return `<div style="margin:0 0 7px;padding:0 0 7px;border-bottom:1px solid ${theme.divider};color:${theme.textPrimary};font-family:'JetBrains Mono Variable','JetBrains Mono',monospace;font-size:11px;font-weight:750;line-height:1.2">${title}</div><div style="line-height:1.55">${lines.join('<br/>')}</div>`
}

export function usageCategoryAxis(labels: string[], theme: UsageChartPalette) {
  return {
    type: 'category' as const,
    data: labels,
    axisLabel: {
      color: theme.textMuted,
      fontSize: 10,
      fontFamily: 'JetBrains Mono Variable, JetBrains Mono, monospace',
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
      fontFamily: 'JetBrains Mono Variable, JetBrains Mono, monospace',
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
      fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: 650,
    },
  }
}

export { tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
