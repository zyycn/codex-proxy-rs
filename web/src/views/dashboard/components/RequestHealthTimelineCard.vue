<script setup lang="ts">
import { computed } from 'vue'

import BaseCard from '../../../components/base/BaseCard.vue'
import type { DashboardHealthTimeline } from '@/api'

const props = defineProps<{
  timeline: DashboardHealthTimeline
}>()

const points = computed(() => props.timeline.points.split(''))

function cellClass(point: string) {
  if (point === '1') return 'bg-[#4FAD7A]'
  if (point === '2') return 'bg-[#D9A13B]'
  if (point === '3') return 'bg-[#D86A62]'
  return 'bg-[#CBD2DC]'
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="w-full px-4 pt-5.5 pb-6 lg:px-7">
    <header class="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
      <div class="pt-0.5">
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">
          {{ timeline.title }}
        </h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">
          {{ timeline.description }}
        </p>
      </div>

      <div class="grid gap-2 lg:justify-items-end">
        <span class="font-mono text-xs leading-none font-[650] text-(--cp-text-secondary)">
          {{ timeline.rangeDisplay }}
        </span>
        <strong class="font-mono text-sm leading-none font-[760] text-(--cp-success-text)">
          {{ timeline.reliabilityDisplay }}
        </strong>
        <div
          class="flex items-center gap-2 text-[11px] leading-none font-[650] text-(--cp-text-muted)"
        >
          <span>{{ timeline.oldestLabel }}</span>
          <i class="size-2 rounded-xs bg-[#CBD2DC]" />
          <i class="size-2 rounded-xs bg-[#D86A62]" />
          <i class="size-2 rounded-xs bg-[#D9A13B]" />
          <i class="size-2 rounded-xs bg-[#4FAD7A]" />
          <span>{{ timeline.newestLabel }}</span>
        </div>
      </div>
    </header>

    <div class="mt-4.25">
      <div class="flex flex-wrap gap-1">
        <i
          v-for="(point, index) in points"
          :key="index"
          class="size-2.5 rounded-[3px]"
          :class="cellClass(point)"
        />
      </div>
    </div>
  </BaseCard>
</template>
