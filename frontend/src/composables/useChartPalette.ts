import { computed } from 'vue'

import { useThemeColor } from '@/composables/useThemeColor'

export function useChartPalette() {
  const color = useThemeColor()
  const palette = computed(() => ({
    textPrimary: color('--cp-color-text', '#0E1726'),
    textSecondary: color('--cp-color-text-secondary', '#64748B'),
    textMuted: color('--cp-color-text-quaternary', '#94A3B8'),
    surface: color('--cp-color-bg-container', '#FFFFFF'),
    surfaceMuted: color('--cp-color-fill-tertiary', '#F1F5F9'),
    grid: color('--cp-color-fill-tertiary', '#F1F5F9'),
    divider: color('--cp-color-split', '#E2E8F0'),
    border: color('--cp-color-border-secondary', '#E2E8F0'),
    pointer: color('--cp-color-border', '#CBD5E1'),
    info: color('--cp-color-info', '#2563EB'),
    success: color('--cp-color-success', '#10B981'),
    warning: color('--cp-color-warning', '#F59E0B'),
    danger: color('--cp-color-error', '#EF4444'),
    normal: color('--cp-color-status-normal', '#0F9F9A'),
    reasoning: color('--cp-color-reasoning', '#8B5CF6'),
  }))

  return { color, palette }
}
