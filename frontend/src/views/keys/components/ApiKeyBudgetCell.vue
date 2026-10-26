<script setup lang="ts">
import type { ApiKey } from '@/api'

import { ZPopover } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { numberLocale } from '@/utils/format'

const props = defineProps<{ apiKey: ApiKey }>()

const { t } = useI18n({ useScope: 'global' })

const windows = computed(() => [
  {
    label: t('keys.day'),
    heading: t('keys.dailyUsage'),
    used: props.apiKey.dailyUsedUsd,
    limit: props.apiKey.dailyLimitUsd,
    reset: props.apiKey.dailyResetsAt,
    resetDisplay: props.apiKey.dailyResetsAtDisplay,
  },
  {
    label: t('keys.week'),
    heading: t('keys.weeklyUsage'),
    used: props.apiKey.weeklyUsedUsd,
    limit: props.apiKey.weeklyLimitUsd,
    reset: props.apiKey.weeklyResetsAt,
    resetDisplay: props.apiKey.weeklyResetsAtDisplay,
  },
])
function amount(value: string) {
  // 列表最多显示两位小数，不补末尾零；明细保留原始金额的全部精度。
  return Number(value).toLocaleString(numberLocale(), { maximumFractionDigits: 2 })
}
</script>

<template>
  <ZPopover class="w-full min-w-0" trigger="hover-click" placement="right" :hover-delay="240">
    <template #reference="{ open }">
      <button
        type="button"
        class="grid w-full min-w-0 cursor-pointer grid-cols-[max-content_minmax(0,1fr)] gap-x-2 gap-y-1 rounded-sm border-0 bg-transparent p-0 text-left text-xs tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :aria-label="t('keys.viewUsageAndCostsFor', { name: apiKey.name })"
        :aria-expanded="open"
        aria-haspopup="dialog"
      >
        <template v-for="window in windows" :key="window.label">
          <span class="text-right text-cp-text-tertiary">{{ window.label }}</span>
          <span class="truncate" :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">
            ${{ amount(window.used) }} / {{ Number(window.limit) === 0 ? '∞' : `$${amount(window.limit)}` }}
          </span>
        </template>
      </button>
    </template>

    <section class="grid min-w-56 max-w-[calc(100vw-1rem)] gap-3 p-3" role="dialog" :aria-label="t('keys.costDetailsTitle')">
      <div v-for="window in windows" :key="window.label" class="grid gap-1">
        <div class="flex items-baseline justify-between gap-6 text-cp-sm">
          <span class="shrink-0 text-cp-text-secondary">{{ window.heading }}</span>
          <span class="min-w-0 break-all text-right font-mono tabular-nums">
            <span :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">${{ window.used }}</span>
            <span class="text-cp-text-tertiary"> / {{ Number(window.limit) === 0 ? '∞' : `$${window.limit}` }}</span>
          </span>
        </div>
        <div class="flex items-baseline justify-between gap-3 text-cp-xs text-cp-text-tertiary">
          <span class="shrink-0">{{ t('common.actions.reset') }}</span>
          <time v-if="window.reset" :datetime="window.reset" class="text-right font-mono tabular-nums">{{ window.resetDisplay ?? '—' }}</time>
          <span v-else>{{ t('keys.confirmNextTime') }}</span>
        </div>
      </div>
    </section>
  </ZPopover>
</template>
