<script setup lang="ts">
import type { UsageDisplayRecord } from '../constants'
import { Archive, ArrowDown, ArrowUp, Info } from '@lucide/vue'

import { computed } from 'vue'
import BasePopover from '@/components/base/BasePopover.vue'
import { usageTokenDetails } from '../constants'

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
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1 font-mono text-[12px] leading-none font-bold tabular-nums"
    >
      <span class="inline-flex items-center gap-1 text-(--cp-success-text)">
        <ArrowDown class="size-3" />
        {{ tokenDetails.inputTokensDisplay }}
      </span>
      <span class="inline-flex items-center gap-1 text-(--cp-info)">
        <ArrowUp class="size-3" />
        {{ tokenDetails.outputTokensDisplay }}
      </span>
      <span class="col-span-2 inline-flex items-center justify-end gap-1 text-(--cp-info)">
        <Archive class="size-3" />
        {{ tokenDetails.cachedTokensDisplay }}
      </span>
    </div>

    <BasePopover
      trigger="hover"
      placement="right"
      width="196px"
      panel-class="!p-3 text-(--cp-text-primary)"
    >
      <template #trigger>
        <button
          type="button"
          class="inline-flex size-4 items-center justify-center rounded-full bg-(--cp-info-bg) text-(--cp-info) outline-none hover:bg-(--cp-default-bg-hover)"
          aria-label="查看 Token 明细"
        >
          <Info class="size-3" />
        </button>
      </template>

      <div class="grid gap-2 text-[12px] leading-none">
        <p class="m-0 font-[760] text-(--cp-text-primary)">
          Token 明细
        </p>
        <div class="grid gap-1.5 text-(--cp-text-secondary)">
          <div v-for="item in tokenItems" :key="item.label" class="flex justify-between gap-4">
            <span class="whitespace-nowrap">{{ item.label }}</span>
            <span class="whitespace-nowrap font-mono font-[760] text-(--cp-text-primary)">
              {{ item.value }}
            </span>
          </div>
        </div>
        <div class="mt-1 flex justify-between border-t border-(--cp-divider-subtle) pt-2">
          <span class="whitespace-nowrap text-(--cp-text-secondary)">总 Token</span>
          <span class="whitespace-nowrap font-mono font-[760] text-(--cp-info-text)">
            {{ tokenDetails.totalTokensDisplay }}
          </span>
        </div>
      </div>
    </BasePopover>
  </div>
</template>
