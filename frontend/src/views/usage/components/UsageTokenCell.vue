<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'
import { Archive, ArrowDown, ArrowUp } from '@lucide/vue'

import { computed } from 'vue'
import { usageTokenDetails } from '../utils/records'
import UsageDetailPopover from './UsageDetailPopover.vue'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const tokenDetails = computed(() => usageTokenDetails(props.record))
const tokenItems = computed(() => [
  { label: '输入 Token', value: tokenDetails.value.inputTokensDisplay },
  { label: '输出 Token', value: tokenDetails.value.outputTokensDisplay },
  { label: '缓存读取 Token', value: tokenDetails.value.cachedTokensDisplay },
  { label: '缓存写入 Token', value: tokenDetails.value.cacheWriteTokensDisplay },
  { label: '推理 Token', value: tokenDetails.value.reasoningTokensDisplay },
])
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <div
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1 font-mono text-cp-sm leading-none font-bold tabular-nums"
    >
      <span class="inline-flex items-center gap-1 text-cp-success-text">
        <ArrowDown class="size-3" />
        {{ tokenDetails.inputTokensDisplay }}
      </span>
      <span class="inline-flex items-center gap-1 text-cp-info-text">
        <ArrowUp class="size-3" />
        {{ tokenDetails.outputTokensDisplay }}
      </span>
      <span class="col-span-2 inline-flex items-center justify-end gap-1 text-cp-info-text">
        <Archive class="size-3" />
        {{ tokenDetails.cachedTokensDisplay }}
      </span>
    </div>

    <UsageDetailPopover title="Token 明细" trigger-label="查看 Token 明细">
      <div class="grid gap-1.5 text-cp-text-secondary">
        <div v-for="item in tokenItems" :key="item.label" class="flex justify-between gap-4">
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="whitespace-nowrap font-mono font-heavy text-cp-text">
            {{ item.value }}
          </span>
        </div>
      </div>
      <div class="mt-1 flex justify-between border-t border-cp-split pt-2">
        <span class="whitespace-nowrap text-cp-text-secondary">总 Token</span>
        <span class="whitespace-nowrap font-mono font-heavy text-cp-info-text">
          {{ tokenDetails.totalTokensDisplay }}
        </span>
      </div>
    </UsageDetailPopover>
  </div>
</template>
