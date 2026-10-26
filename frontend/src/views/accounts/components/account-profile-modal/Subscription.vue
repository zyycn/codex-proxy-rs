<script setup lang="ts">
import type { AccountSubscription } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  subscription: AccountSubscription
}>()

const { t } = useI18n({ useScope: 'global' })

const renewal = computed(() => {
  if (props.subscription?.willRenew === true)
    return t('accounts.profile.subscription.automaticRenewal')
  if (props.subscription?.willRenew === false)
    return t('accounts.profile.subscription.noAutomaticRenewal')
  return t('common.values.unknown')
})
const billingPeriod = computed(() => {
  const period = props.subscription?.billingPeriod
  if (!period)
    return null
  const labels: Record<string, string> = {
    month: t('accounts.profile.subscription.byMonth'),
    monthly: t('accounts.profile.subscription.byMonth'),
    year: t('accounts.profile.subscription.byYear'),
    yearly: t('accounts.profile.subscription.byYear'),
    annual: t('accounts.profile.subscription.byYear'),
  }
  return labels[period.toLowerCase()] ?? period
})
const billing = computed(() => [billingPeriod.value, props.subscription?.billingCurrency].filter(Boolean).join(' · ') || t('common.values.unknown'))
const dates = computed(() => [
  {
    label: t('accounts.profile.subscription.subscriptionStarts'),
    value: props.subscription?.startsAt,
    display: props.subscription?.startsAtDisplay,
  },
  {
    label: t('accounts.profile.subscription.subscriptionEnds'),
    value: props.subscription?.expiresAt,
    display: props.subscription?.expiresAtDisplay,
  },
])
</script>

<template>
  <section class="min-w-0 rounded-cp-lg bg-cp-fill-alter p-4 sm:p-5" :aria-label="t('accounts.profile.subscription.label')">
    <dl class="m-0 grid grid-cols-2 gap-x-6 gap-y-4 sm:grid-cols-[1.3fr_1.3fr_1fr_1fr]">
      <div v-for="date in dates" :key="date.label" class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          {{ date.label }}
        </dt>
        <dd class="m-0 mt-1.5 font-mono text-cp-sm leading-relaxed tabular-nums text-cp-text">
          <time v-if="date.value" :datetime="date.value">{{ date.display ?? t('common.values.unknown') }}</time>
          <span v-else>{{ t('common.values.unknown') }}</span>
        </dd>
      </div>
      <div class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          {{ t('accounts.profile.subscription.renewalMethod') }}
        </dt>
        <dd class="m-0 mt-1.5 text-cp-sm leading-relaxed" :class="subscription.willRenew === true ? 'text-cp-success' : 'text-cp-text'">
          {{ renewal }}
        </dd>
      </div>
      <div class="min-w-0">
        <dt class="text-cp-xs text-cp-text-secondary">
          {{ t('accounts.usage.billing') }}
        </dt>
        <dd class="m-0 mt-1.5 text-cp-sm leading-relaxed wrap-break-word text-cp-text">
          {{ billing }}
        </dd>
      </div>
    </dl>
  </section>
</template>
