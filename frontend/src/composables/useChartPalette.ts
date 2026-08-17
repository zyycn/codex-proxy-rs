import { computed } from 'vue'

import { useThemeColor } from '@/composables/useThemeColor'

export function useChartPalette() {
  const color = useThemeColor()
  const palette = computed(() => ({
    textPrimary: color('--cp-text-primary', '#0E1726'),
    textSecondary: color('--cp-text-secondary', '#64748B'),
    textMuted: color('--cp-text-muted', '#94A3B8'),
    surface: color('--cp-bg-surface', '#FFFFFF'),
    surfaceMuted: color('--cp-bg-muted', '#F1F5F9'),
    grid: color('--cp-bg-muted', '#F1F5F9'),
    divider: color('--cp-divider-subtle', '#E2E8F0'),
    border: color('--cp-default-border', '#E2E8F0'),
    pointer: color('--cp-default-border-hover', '#CBD5E1'),
    info: color('--cp-info', '#2563EB'),
    success: color('--cp-success', '#10B981'),
    warning: color('--cp-warning', '#F59E0B'),
    danger: color('--cp-danger', '#EF4444'),
    normal: color('--cp-normal', '#0F9F9A'),
    reasoning: color('--cp-reasoning', '#8B5CF6'),
  }))

  return { color, palette }
}
