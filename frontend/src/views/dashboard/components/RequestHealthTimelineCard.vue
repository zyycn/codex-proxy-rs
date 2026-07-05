<script setup lang="ts">
import { computed } from 'vue'

import BaseCard from '../../../components/base/BaseCard.vue'

const props = defineProps<{
  timeline: any
}>()

const healthLegend = [
  { code: '0', label: '未来' },
  { code: '1', label: '无请求' },
  { code: '2', label: '不可达' },
  { code: '3', label: '不稳定' },
  { code: '4', label: '低样本' },
  { code: '5', label: '稳定' },
]

const points = computed(() => String(props.timeline.points ?? '').split(''))
const pointGroups = computed(() => {
  const groups: string[][] = []
  for (let index = 0; index < points.value.length; index += 4) {
    groups.push(points.value.slice(index, index + 4))
  }
  return groups
})

function cellClass(point: string) {
  if (point === '0') return 'bg-(--cp-disabled-bg) opacity-60'
  if (point === '1') return 'bg-(--cp-default-border-hover)'
  if (point === '2') return 'bg-(--cp-danger)'
  if (point === '3') return 'bg-(--cp-warning)'
  if (point === '4') return 'bg-(--cp-normal)'
  if (point === '5') return 'bg-(--cp-success)'
  return 'bg-(--cp-default-border-hover)'
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
      <div class="flex w-full flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div
          class="flex max-w-full flex-wrap items-center gap-x-2 gap-y-1 text-[11px] leading-none font-[650] text-(--cp-text-muted)"
        >
          <span
            v-for="item in healthLegend"
            :key="item.code"
            class="inline-flex h-3.5 items-center gap-1 align-middle leading-none"
          >
            <span
              aria-hidden="true"
              class="block size-2 shrink-0 rounded-xs"
              :class="cellClass(item.code)"
            />
            <span class="block leading-none">{{ item.label }}</span>
          </span>
        </div>
        <strong
          class="shrink-0 font-mono text-sm leading-none font-[760] tabular-nums text-(--cp-success-text)"
        >
          {{ timeline.reliabilityDisplay }}
        </strong>
      </div>
    </template>

    <template #body>
      <div class="mt-4.25">
        <div class="grid w-full grid-cols-24 items-end gap-0.75">
          <div
            v-for="(group, groupIndex) in pointGroups"
            :key="groupIndex"
            class="grid grid-cols-4 items-end gap-0.5"
          >
            <i
              v-for="(point, pointIndex) in group"
              :key="groupIndex * 4 + pointIndex"
              class="h-3.5 min-w-0.5 rounded-xs"
              :class="cellClass(point)"
            />
          </div>
        </div>
      </div>
    </template>
  </BaseCard>
</template>
