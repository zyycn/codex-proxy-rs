import { storeToRefs } from 'pinia'
import { computed } from 'vue'

import { useUiStore } from '@/stores/modules/ui'
import { readCssVariable } from '@/utils/css'

export function useUsageChartPalette() {
  const { themeRevision } = storeToRefs(useUiStore())

  const palette = computed(() => {
    void themeRevision.value
    return {
      textPrimary: readCssVariable('--cp-text-primary', '#0E1726'),
      textSecondary: readCssVariable('--cp-text-secondary', '#64748B'),
      textMuted: readCssVariable('--cp-text-muted', '#94A3B8'),
      surface: readCssVariable('--cp-bg-surface', '#FFFFFF'),
      grid: readCssVariable('--cp-bg-muted', '#F1F5F9'),
      pointer: readCssVariable('--cp-default-border-hover', '#CBD5E1'),
      info: readCssVariable('--cp-info', '#2563EB'),
      success: readCssVariable('--cp-success', '#12B981'),
      warning: readCssVariable('--cp-warning', '#F59E0B'),
      danger: readCssVariable('--cp-danger', '#EF4444'),
      normal: readCssVariable('--cp-normal', '#0F9F9A'),
    }
  })

  return { palette }
}
