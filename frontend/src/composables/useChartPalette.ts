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
    info: color('--cp-color-blue-solid', '#5983F4'),
    success: color('--cp-color-green-solid', '#12B981'),
    warning: color('--cp-color-orange-solid', '#F59E0B'),
    danger: color('--cp-color-red-solid', '#EF4444'),
    normal: color('--cp-color-text-tertiary', '#94A3B8'),
    reasoning: color('--cp-color-purple-solid', '#722ED1'),
  }))

  return { color, palette }
}
