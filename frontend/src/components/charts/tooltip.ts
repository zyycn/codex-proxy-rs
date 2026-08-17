import { isRecord } from '@/utils/object'

interface ChartTooltipTheme {
  surface: string
  textPrimary: string
  pointer: string
}

interface ChartTooltipStyleOptions {
  axisPointer?: boolean
  confine?: boolean
  padding?: [number, number]
  fontFamily?: string
  fontWeight?: number
}

export function chartTooltipStyle(
  theme: ChartTooltipTheme,
  options: ChartTooltipStyleOptions = {},
) {
  const base = {
    confine: options.confine,
    backgroundColor: theme.surface,
    borderColor: 'transparent',
    borderWidth: 0,
    padding: options.padding ?? [10, 14],
    textStyle: {
      color: theme.textPrimary,
      fontSize: 12,
      fontFamily: options.fontFamily ?? 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: options.fontWeight ?? 650,
    },
    extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
  }

  if (!options.axisPointer)
    return base

  return {
    ...base,
    axisPointer: {
      type: 'line' as const,
      lineStyle: { color: theme.pointer, type: 'dashed' as const, width: 1 },
    },
  }
}

export function tooltipRows(params: unknown): Record<string, unknown>[] {
  const values = Array.isArray(params) ? params : [params]
  return values.filter(isRecord)
}

export function tooltipIndex(source: unknown) {
  if (!isRecord(source))
    return -1
  return typeof source.dataIndex === 'number' ? source.dataIndex : -1
}
