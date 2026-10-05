<script setup lang="ts">
import type { AccountSubscription } from '@/api'
import { computed } from 'vue'

const props = defineProps<{
  subscription: AccountSubscription
}>()

const renewal = computed(() => {
  if (props.subscription?.willRenew === true)
    return '自动续费'
  if (props.subscription?.willRenew === false)
    return '不自动续费'
  return '未知'
})
const billingPeriod = computed(() => {
  const period = props.subscription?.billingPeriod
  if (!period)
    return null
  const labels: Record<string, string> = { month: '按月', monthly: '按月', year: '按年', yearly: '按年', annual: '按年' }
  return labels[period.toLowerCase()] ?? period
})
const billing = computed(() => [billingPeriod.value, props.subscription?.billingCurrency].filter(Boolean).join(' · ') || '未知')
const dates = computed(() => [
  { label: '订阅开始', value: props.subscription?.startsAt, display: props.subscription?.startsAtDisplay },
  { label: '订阅结束', value: props.subscription?.expiresAt, display: props.subscription?.expiresAtDisplay },
])
</script>

<template>
  <section class="min-w-0 rounded-cp-lg bg-cp-fill-alter p-4 sm:p-5" aria-label="订阅">
    <dl class="m-0 grid grid-cols-2 gap-x-6 gap-y-4 sm:grid-cols-[1.3fr_1.3fr_1fr_1fr]">
      <div v-for="date in dates" :key="date.label" class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          {{ date.label }}
        </dt>
        <dd class="m-0 mt-1.5 font-mono text-cp-sm leading-relaxed tabular-nums text-cp-text">
          <time v-if="date.value" :datetime="date.value">{{ date.display ?? '未知' }}</time>
          <span v-else>未知</span>
        </dd>
      </div>
      <div class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          续费方式
        </dt>
        <dd class="m-0 mt-1.5 text-cp-sm leading-relaxed" :class="subscription.willRenew === true ? 'text-cp-success' : 'text-cp-text'">
          {{ renewal }}
        </dd>
      </div>
      <div class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          计费
        </dt>
        <dd class="m-0 mt-1.5 text-cp-sm leading-relaxed break-words text-cp-text">
          {{ billing }}
        </dd>
      </div>
    </dl>
  </section>
</template>
