<script setup lang="ts">
import { computed } from 'vue'

import BaseCard from '../../../components/base/BaseCard.vue'

const props = defineProps<{
  timeline: any
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
  <BaseCard
    as="article"
    variant="dashboard"
    :title="timeline.title"
    :description="timeline.description"
    header-collapse-at="lg"
    class="w-full"
  >
    <template #actions>
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
    </template>

    <template #body>
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
    </template>
  </BaseCard>
</template>
