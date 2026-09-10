<script setup lang="ts">
import type { ApiKey } from '@/api'
import { computed } from 'vue'
import BasePopover from '@/components/base/BasePopover.vue'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{ apiKey: ApiKey }>()
const windows = computed(() => [
  { label: '日', heading: '日用量', used: props.apiKey.dailyUsedUsd, limit: props.apiKey.dailyLimitUsd, reset: props.apiKey.dailyResetsAt },
  { label: '周', heading: '周用量', used: props.apiKey.weeklyUsedUsd, limit: props.apiKey.weeklyLimitUsd, reset: props.apiKey.weeklyResetsAt },
])
function amount(value: string) {
  // 列表显示两位小数，明细直接使用原始字符串，保留全部金额精度。
  return Number(value).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
}
</script>

<template>
  <BasePopover class="w-full min-w-0" trigger="hover-click" placement="right" :hover-delay="240">
    <template #trigger="{ open }">
      <button
        type="button"
        class="grid w-full min-w-0 cursor-pointer gap-1 rounded-sm border-0 bg-transparent p-0 text-left text-xs tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :aria-label="`查看 ${apiKey.name} 的费用用量`"
        :aria-expanded="open"
        aria-haspopup="dialog"
      >
        <span v-for="window in windows" :key="window.label" class="flex min-w-0 items-center gap-2">
          <span class="shrink-0 text-cp-text-tertiary">{{ window.label }}</span>
          <span class="truncate" :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">
            ${{ amount(window.used) }} / {{ Number(window.limit) === 0 ? '∞' : `$${amount(window.limit)}` }}
          </span>
        </span>
      </button>
    </template>

    <section class="grid min-w-56 max-w-[calc(100vw-1rem)] gap-3 p-3" role="dialog" aria-label="费用用量详情（美元）">
      <div v-for="window in windows" :key="window.label" class="grid gap-1">
        <div class="flex items-baseline justify-between gap-6 text-cp-sm">
          <span class="shrink-0 text-cp-text-secondary">{{ window.heading }}</span>
          <span class="min-w-0 break-all text-right font-mono tabular-nums">
            <span :class="Number(window.limit) > 0 && Number(window.used) >= Number(window.limit) ? 'text-cp-error' : 'text-cp-text'">${{ window.used }}</span>
            <span class="text-cp-text-tertiary"> / {{ Number(window.limit) === 0 ? '∞' : `$${window.limit}` }}</span>
          </span>
        </div>
        <div class="flex items-baseline justify-between gap-3 text-cp-xs text-cp-text-tertiary">
          <span class="shrink-0">{{ window.reset ? '重置（北京时间）' : '重置' }}</span>
          <time v-if="window.reset" :datetime="window.reset" class="text-right font-mono tabular-nums">{{ formatDateTime(window.reset, '—', 'Asia/Shanghai') }}</time>
          <span v-else>下次使用时确定</span>
        </div>
      </div>
    </section>
  </BasePopover>
</template>
