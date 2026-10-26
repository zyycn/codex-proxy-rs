<script setup lang="ts">
import type { UsageListRecord } from '@/api'

import { Zap } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import UsageDetailPopover from '@/components/usage/UsageDetailPopover.vue'
import { usageBillingText } from './shared/presenter'

const props = defineProps<{
  record: Pick<UsageListRecord, 'billing'>
}>()

const { t } = useI18n({ useScope: 'global' })

const billing = computed(() => props.record.billing)
const billingItems = computed(() => {
  const value = billing.value
  if (!value)
    return []

  return [
    {
      label: t('usage.billing.serviceLevel'),
      value: value.serviceTierDisplay,
      tone: 'info',
    },
    {
      label: t('settings.pricing.multiplier'),
      value: value.multiplierDisplay,
      tone: 'info',
    },
    {
      label: t('usage.billing.totalCost'),
      value: value.totalAmountDisplay,
      tone: 'success',
    },
    {
      label: t('usage.billing.standardFee'),
      value: value.standardAmountDisplay,
      tone: 'default',
    },
  ]
})

const amountItems = computed(() => {
  const value = billing.value
  if (!value)
    return []

  return [
    {
      label: value.image
        ? t('usage.billing.textInputCost')
        : t('usage.billing.inputCost'),
      value: value.inputAmountDisplay,
      accent: false,
    },
    ...(value.image
      ? [
          {
            label: t('usage.billing.imageInputFee'),
            value: value.image.inputAmountDisplay,
            accent: false,
          },
          {
            label: t('usage.billing.imageCachingCharges'),
            value: value.image.cacheReadAmountDisplay,
            accent: false,
          },
          {
            label: t('usage.billing.imageInputUnitPrice'),
            value: value.image.inputPriceDisplay,
            accent: true,
          },
          {
            label: t('usage.billing.imageCacheUnitPrice'),
            value: value.image.cacheReadPriceDisplay,
            accent: true,
          },
        ]
      : []),
    {
      label: value.image
        ? t('usage.billing.imageOutputFee')
        : t('usage.billing.outputFee'),
      value: value.outputAmountDisplay,
      accent: false,
    },
    {
      label: value.image
        ? t('usage.billing.textInputUnitPrice')
        : t('usage.billing.inputUnitPrice'),
      value: value.inputPriceDisplay,
      accent: true,
    },
    {
      label: value.image
        ? t('usage.billing.imageOutputUnitPrice')
        : t('usage.billing.outputUnitPrice'),
      value: value.outputPriceDisplay,
      accent: true,
    },
    {
      label: value.image
        ? t('usage.billing.textCacheReadCost')
        : t('usage.billing.cacheReadCost'),
      value: value.cacheReadAmountDisplay,
      accent: false,
    },
    {
      label: t('usage.billing.cacheWriteCost'),
      value: value.cacheWriteAmountDisplay,
      accent: false,
    },
    {
      label: t('usage.billing.cacheWriteUnitPrice'),
      value: value.cacheWritePriceDisplay,
      accent: true,
    },
  ]
})

function itemValueClass(tone?: string, accent?: boolean) {
  if (tone === 'success')
    return 'text-cp-success-text'
  if (tone === 'info' || accent)
    return 'text-cp-info-text'
  return 'text-cp-text'
}
</script>

<template>
  <div class="flex items-center justify-end gap-1.5">
    <span class="font-mono text-cp font-heavy tabular-nums text-cp-green-text">
      {{ usageBillingText(record) }}
    </span>

    <div v-if="billing" class="flex items-center gap-1">
      <span
        v-if="billing.serviceTierDisplay === 'Fast'"
        class="inline-flex size-4 items-center justify-center rounded-full bg-cp-warning-container text-cp-warning-on-container"
        :title="t('usage.billing.fastMode')"
        role="img"
        :aria-label="t('usage.billing.fastMode')"
      >
        <Zap class="size-3" stroke-width="2.2" aria-hidden="true" />
      </span>
      <UsageDetailPopover
        :title="billing.longContextBillingApplied ? t('usage.billing.longContextBillingDetails') : t('usage.billing.billingDetails')"
        :trigger-label="billing.longContextBillingApplied ? t('usage.billing.viewLongContextBillingDetails') : t('usage.billing.viewCostDetails')"
        :tone="billing.longContextBillingApplied ? 'warning' : 'primary'"
      >
        <div class="grid gap-1.5 text-cp-text-secondary">
          <div v-for="item in amountItems" :key="item.label" class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-4">
            <span class="whitespace-nowrap">{{ item.label }}</span>
            <span class="justify-self-end whitespace-nowrap font-mono font-heavy" :class="itemValueClass(undefined, item.accent)">
              {{ item.value }}
            </span>
          </div>
        </div>
        <div class="mt-1 grid gap-1.5 rounded-cp bg-cp-fill-tertiary p-2 text-cp-text-secondary">
          <div v-for="item in billingItems" :key="item.label" class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-4">
            <span class="whitespace-nowrap">{{ item.label }}</span>
            <span class="justify-self-end whitespace-nowrap font-mono font-heavy" :class="itemValueClass(item.tone)">
              {{ item.value }}
            </span>
          </div>
        </div>
      </UsageDetailPopover>
    </div>
  </div>
</template>
