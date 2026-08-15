<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'

import { computed } from 'vue'
import { usageLatencyDetails } from '../utils/records'
import UsageDetailPopover from './UsageDetailPopover.vue'

const props = defineProps<{
  record: UsageDisplayRecord
}>()

const latencyDetails = computed(() => usageLatencyDetails(props.record))
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <div
      class="grid grid-cols-[auto_auto] items-center justify-end gap-x-2 gap-y-1.5 font-mono text-[12px] leading-none font-heavy tabular-nums"
    >
      <span class="text-[11px] text-cp-muted-text">{{ latencyDetails.firstOutputLabel }}</span>
      <span class="text-cp-secondary">{{ latencyDetails.firstOutputDisplay }}</span>
      <span class="text-[11px] text-cp-muted-text">总耗时</span>
      <span class="text-cp-primary">{{ latencyDetails.totalDisplay }}</span>
    </div>

    <UsageDetailPopover title="延迟明细" trigger-label="查看延迟明细">
      <div
        v-if="latencyDetails.breakdownItems.length"
        class="grid gap-1.5 text-cp-secondary"
      >
        <div
          v-for="item in latencyDetails.breakdownItems"
          :key="item.label"
          class="flex justify-between gap-4"
        >
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="whitespace-nowrap font-mono font-heavy text-cp-primary">
            {{ item.value }}
          </span>
        </div>
      </div>
      <p v-else class="m-0 text-cp-muted-text">
        此记录未采集完整的阶段耗时。
      </p>

      <div class="flex justify-between border-t border-cp-divider pt-2">
        <span class="whitespace-nowrap text-cp-secondary">总耗时</span>
        <span class="whitespace-nowrap font-mono font-heavy text-cp-info-text">
          {{ latencyDetails.totalDisplay }}
        </span>
      </div>

      <div
        v-if="latencyDetails.transportItems.length"
        class="grid gap-1.5 border-t border-cp-divider pt-2 text-cp-secondary"
      >
        <p class="m-0 font-heavy text-cp-primary">
          传输观测
        </p>
        <div
          v-for="item in latencyDetails.transportItems"
          :key="item.label"
          class="flex justify-between gap-4"
        >
          <span class="whitespace-nowrap">{{ item.label }}</span>
          <span class="whitespace-nowrap font-mono font-heavy text-cp-primary">
            {{ item.value }}
          </span>
        </div>
        <p class="m-0 text-[11px] leading-snug text-cp-muted-text">
          与阶段耗时可能重叠，不参与总耗时相加。
        </p>
      </div>
    </UsageDetailPopover>
  </div>
</template>
