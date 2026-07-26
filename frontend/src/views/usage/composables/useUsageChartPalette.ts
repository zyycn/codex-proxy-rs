import { computed } from 'vue'

import { useThemeColor } from '@/composables/useThemeColor'

export function useUsageChartPalette() {
  const themeColor = useThemeColor()

  const palette = computed(() => ({
    textPrimary: themeColor('--cp-text-primary', '#0E1726'),
    textSecondary: themeColor('--cp-text-secondary', '#64748B'),
    textMuted: themeColor('--cp-text-muted', '#94A3B8'),
    surface: themeColor('--cp-bg-surface', '#FFFFFF'),
    grid: themeColor('--cp-bg-muted', '#F1F5F9'),
    pointer: themeColor('--cp-default-border-hover', '#CBD5E1'),
    info: themeColor('--cp-info', '#2563EB'),
    success: themeColor('--cp-success', '#12B981'),
    warning: themeColor('--cp-warning', '#F59E0B'),
    danger: themeColor('--cp-danger', '#EF4444'),
    normal: themeColor('--cp-normal', '#0F9F9A'),
  }))

  return { palette }
}
