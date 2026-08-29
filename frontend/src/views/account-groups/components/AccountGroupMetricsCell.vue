<script setup lang="ts">
import type { Component } from 'vue'
import type { AccountGroup } from '@/api'
import { AlertTriangle, CircleCheck, Grid2X2, Users } from '@lucide/vue'
import { computed } from 'vue'

import { formatUsd } from '@/views/usage/utils/format'

type MetricKind = 'accounts' | 'capacity' | 'usage'
type MetricTone = 'primary' | 'secondary' | 'success' | 'warning' | 'active'

interface MetricItem {
  label: string
  value: string | number
  tone: MetricTone
  icon?: Component
}

const props = defineProps<{
  group: AccountGroup
  kind: MetricKind
}>()

const toneClasses: Record<MetricTone, string> = {
  primary: 'text-cp-text',
  secondary: 'text-cp-text-secondary',
  success: 'text-cp-success-text',
  warning: 'text-cp-warning',
  active: 'text-cp-primary-text',
}

const metrics = computed<MetricItem[]>(() => {
  if (props.kind === 'accounts') {
    return [
      {
        label: '可用',
        value: props.group.accountSummary.available,
        tone: 'success',
        icon: CircleCheck,
      },
      {
        label: '受限',
        value: props.group.accountSummary.limited,
        tone: 'warning',
        icon: AlertTriangle,
      },
      {
        label: '总数',
        value: props.group.accountSummary.total,
        tone: 'primary',
        icon: Users,
      },
    ]
  }

  if (props.kind === 'capacity') {
    return [
      {
        label: '并发',
        value: `${props.group.capacity.usedSlots ?? '—'} / ${props.group.capacity.totalSlots}`,
        tone: (props.group.capacity.usedSlots ?? 0) > 0 ? 'active' : 'primary',
        icon: Grid2X2,
      },
    ]
  }

  return [
    { label: '今日', value: formatUsd(props.group.usage.todayUsd), tone: 'primary' },
    { label: '累计', value: formatUsd(props.group.usage.totalUsd), tone: 'secondary' },
  ]
})
</script>

<template>
  <dl v-if="kind === 'usage'" class="m-0 grid grid-cols-[auto_auto] justify-start gap-x-2 gap-y-0.5 text-xs leading-4">
    <template v-for="metric in metrics" :key="metric.label">
      <dt class="text-cp-text-quaternary">
        {{ metric.label }}
      </dt>
      <dd class="m-0 font-mono font-semibold tabular-nums" :class="toneClasses[metric.tone]">
        {{ metric.value }}
      </dd>
    </template>
  </dl>

  <dl v-else class="m-0 flex items-center gap-2.5 whitespace-nowrap text-xs leading-none">
    <div v-for="metric in metrics" :key="metric.label" class="inline-flex min-w-0 items-center gap-1">
      <dt class="flex items-center" :class="toneClasses[metric.tone]" :title="metric.label">
        <component :is="metric.icon" v-if="metric.icon" class="size-3 shrink-0" />
        <span class="sr-only">{{ metric.label }}</span>
      </dt>
      <dd class="m-0 font-mono font-semibold tabular-nums" :class="toneClasses[metric.tone]">
        {{ metric.value }}
      </dd>
    </div>
  </dl>
</template>
