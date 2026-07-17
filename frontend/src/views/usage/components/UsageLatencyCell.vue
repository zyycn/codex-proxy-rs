<script setup lang="ts">
import type { UsageDisplayRecord } from '../constants'
import { Info } from '@lucide/vue'

import { computed } from 'vue'
import BasePopover from '@/components/base/BasePopover.vue'
import { usageLatencyDetails } from '../constants'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const latencyDetails = computed(() => usageLatencyDetails(props.record))
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <div
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1.5 font-mono text-[12px] leading-none font-[760] tabular-nums"
    >
      <span class="text-[11px] text-(--cp-text-muted)">首字</span>
      <span class="text-(--cp-text-secondary)">{{ latencyDetails.firstTokenDisplay }}</span>
      <span class="text-[11px] text-(--cp-text-muted)">总耗时</span>
      <span class="text-(--cp-text-primary)">{{ latencyDetails.totalDisplay }}</span>
    </div>

    <BasePopover
      trigger="hover"
      placement="right"
      width="252px"
      panel-class="!p-3 text-(--cp-text-primary)"
    >
      <template #trigger>
        <button
          type="button"
          class="inline-flex size-4 items-center justify-center rounded-full bg-(--cp-info-bg) text-(--cp-info) outline-none hover:bg-(--cp-default-bg-hover)"
          aria-label="查看延迟明细"
        >
          <Info class="size-3" />
        </button>
      </template>

      <div class="grid gap-2 text-[12px] leading-none">
        <p class="m-0 font-[760] text-(--cp-text-primary)">
          延迟明细
        </p>

        <div
          v-if="latencyDetails.breakdownItems.length"
          class="grid gap-1.5 text-(--cp-text-secondary)"
        >
          <div
            v-for="item in latencyDetails.breakdownItems"
            :key="item.label"
            class="flex justify-between gap-4"
          >
            <span class="whitespace-nowrap">{{ item.label }}</span>
            <span class="whitespace-nowrap font-mono font-[760] text-(--cp-text-primary)">
              {{ item.value }}
            </span>
          </div>
        </div>
        <p v-else class="m-0 text-(--cp-text-muted)">
          此记录未采集完整的阶段耗时。
        </p>

        <div class="flex justify-between border-t border-(--cp-divider-subtle) pt-2">
          <span class="whitespace-nowrap text-(--cp-text-secondary)">总耗时</span>
          <span class="whitespace-nowrap font-mono font-[760] text-(--cp-info-text)">
            {{ latencyDetails.totalDisplay }}
          </span>
        </div>

        <div
          v-if="latencyDetails.transportItems.length"
          class="grid gap-1.5 border-t border-(--cp-divider-subtle) pt-2 text-(--cp-text-secondary)"
        >
          <p class="m-0 font-[760] text-(--cp-text-primary)">
            传输观测
          </p>
          <div
            v-for="item in latencyDetails.transportItems"
            :key="item.label"
            class="flex justify-between gap-4"
          >
            <span class="whitespace-nowrap">{{ item.label }}</span>
            <span class="whitespace-nowrap font-mono font-[760] text-(--cp-text-primary)">
              {{ item.value }}
            </span>
          </div>
          <p class="m-0 text-[11px] leading-snug text-(--cp-text-muted)">
            与阶段耗时可能重叠，不参与总耗时相加。
          </p>
        </div>
      </div>
    </BasePopover>
  </div>
</template>
