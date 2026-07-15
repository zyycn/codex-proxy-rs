<script setup lang="ts">
import { computed } from 'vue'

import {
  formatHealthCount,
  healthReliabilityValueClass,
  healthStatusMeta,
  type HealthTimelinePoint,
} from '../constants'

const props = defineProps<{
  point: HealthTimelinePoint
}>()

const status = computed(() => healthStatusMeta[props.point.status])
const eligibleRequests = computed(() => props.point.successRequests + props.point.failedRequests)
const observedRequests = computed(
  () => eligibleRequests.value + props.point.cancelledRequests + props.point.callerErrorRequests,
)
const outcomeSegments = computed(() => {
  const total = observedRequests.value
  if (total === 0) return []

  return [
    {
      label: '成功',
      value: props.point.successRequests,
      className: 'bg-(--cp-success)',
    },
    {
      label: '服务失败',
      value: props.point.failedRequests,
      className: 'bg-(--cp-danger)',
    },
    {
      label: '客户端取消',
      value: props.point.cancelledRequests,
      className: 'bg-(--cp-normal)',
    },
    {
      label: '调用方错误',
      value: props.point.callerErrorRequests,
      className: 'bg-(--cp-info)',
    },
  ]
    .filter((item) => item.value > 0)
    .map((item) => ({
      ...item,
      percentage: (item.value / total) * 100,
    }))
})
const metricItems = computed(() => [
  {
    label: '成功',
    value: props.point.successRequests,
    dotClass: 'bg-(--cp-success)',
    valueClass: 'text-(--cp-success-text)',
  },
  {
    label: '服务失败',
    value: props.point.failedRequests,
    dotClass: 'bg-(--cp-danger)',
    valueClass: 'text-(--cp-danger-text)',
  },
  {
    label: '客户端取消',
    value: props.point.cancelledRequests,
    dotClass: 'bg-(--cp-normal)',
    valueClass: 'text-(--cp-normal-text)',
  },
  {
    label: '调用方错误',
    value: props.point.callerErrorRequests,
    dotClass: 'bg-(--cp-info)',
    valueClass: 'text-(--cp-info-text)',
  },
])
</script>

<template>
  <section
    role="dialog"
    :aria-label="`${point.time} 请求健康详情`"
    class="overflow-hidden rounded-(--cp-popover-radius)"
  >
    <header class="flex items-center justify-between gap-3 bg-(--cp-bg-subtle) px-3.5 py-3">
      <div class="flex min-w-0 items-center gap-2">
        <span class="size-2 shrink-0 rounded-full" :class="status.cellClass" />
        <strong class="font-mono text-[13px] leading-none font-[780] text-(--cp-text-primary)">
          {{ point.time }}
        </strong>
        <span
          class="inline-flex h-5 items-center rounded-full px-2 text-[10px] leading-none font-[760]"
          :class="status.badgeClass"
        >
          {{ status.label }}
        </span>
      </div>
      <span class="shrink-0 text-[10px] leading-none font-[650] text-(--cp-text-muted)">
        15 分钟
      </span>
    </header>

    <div class="grid gap-3.5 px-3.5 py-3.5">
      <div class="flex items-end justify-between gap-4">
        <div>
          <p class="m-0 text-[11px] leading-none font-[650] text-(--cp-text-muted)">
            有效请求可用性
          </p>
          <strong
            class="mt-2 block font-mono text-[25px] leading-none font-[790] tabular-nums"
            :class="healthReliabilityValueClass(point.successRequests, point.failedRequests)"
          >
            {{ point.reliabilityDisplay }}
          </strong>
        </div>
        <div class="text-right">
          <span class="block text-[10px] leading-none font-[650] text-(--cp-text-muted)">
            有效请求
          </span>
          <strong
            class="mt-2 block font-mono text-[15px] leading-none font-[760] tabular-nums text-(--cp-text-primary)"
          >
            {{ formatHealthCount(eligibleRequests) }}
          </strong>
        </div>
      </div>

      <div
        class="flex h-1.5 w-full overflow-hidden rounded-full bg-(--cp-bg-muted)"
        aria-hidden="true"
      >
        <span
          v-for="segment in outcomeSegments"
          :key="segment.label"
          class="h-full"
          :class="segment.className"
          :style="{ flexBasis: `${segment.percentage}%` }"
        />
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div
          v-for="item in metricItems"
          :key="item.label"
          class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-2.5 py-2.5"
        >
          <span class="size-1.5 rounded-full" :class="item.dotClass" />
          <span class="truncate text-[10px] leading-none font-[650] text-(--cp-text-secondary)">
            {{ item.label }}
          </span>
          <strong
            class="font-mono text-[12px] leading-none font-[760] tabular-nums"
            :class="item.valueClass"
          >
            {{ formatHealthCount(item.value) }}
          </strong>
        </div>
      </div>

      <p
        class="m-0 border-t border-(--cp-divider-subtle) pt-3 text-[10px] leading-[1.45] font-[650] text-(--cp-text-muted)"
      >
        客户端取消与调用方错误单独记录，不计入可用性。
      </p>
    </div>
  </section>
</template>
