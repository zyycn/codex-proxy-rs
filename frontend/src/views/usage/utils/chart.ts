import type { useUsageChartPalette } from '../composables/useUsageChartPalette'

type UsageChartPalette = ReturnType<typeof useUsageChartPalette>['palette']['value']

export function usageTooltip(
  theme: UsageChartPalette,
  formatter: (params: unknown) => string,
) {
  return {
    trigger: 'axis' as const,
    backgroundColor: theme.surface,
    borderColor: 'transparent',
    borderWidth: 0,
    padding: [10, 14],
    textStyle: {
      color: theme.textPrimary,
      fontSize: 12,
      fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: 650,
    },
    extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
    axisPointer: {
      type: 'line' as const,
      lineStyle: { color: theme.pointer, type: 'dashed' as const, width: 1 },
    },
    formatter,
  }
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

export function tooltipRows(params: unknown): Record<string, unknown>[] {
  const values = Array.isArray(params) ? params : [params]
  return values.filter(
    (value): value is Record<string, unknown> => typeof value === 'object' && value !== null,
  )
}

export function tooltipIndex(source: unknown) {
  if (typeof source !== 'object' || source === null || !('dataIndex' in source))
    return -1
  return typeof source.dataIndex === 'number' ? source.dataIndex : -1
}
