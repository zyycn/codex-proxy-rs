<script setup lang="ts">
import type { AccountQuotaCredits } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { numberLocale } from '@/utils/format'

const props = defineProps<{
  credits: AccountQuotaCredits | null
}>()

const { t } = useI18n({ useScope: 'global' })

const balanceDisplay = computed(() => {
  const balance = props.credits?.balance?.trim()
  const decimal = balance?.match(/^(-?)(\d+)(?:\.(\d+))?$/)
  if (!decimal)
    return null
  const [, sign, integer, fraction] = decimal
  // 点数可能包含高精度小数，整数分组与小数文本分别处理，避免转成浮点数。
  const grouped = `${sign}${new Intl.NumberFormat(numberLocale()).format(BigInt(integer!))}`
  return fraction
    ? `${grouped}.${fraction}`
    : grouped
})
const value = computed(() => {
  if (props.credits?.unlimited)
    return t('accounts.quota.unlimited')
  if (balanceDisplay.value !== null)
    return balanceDisplay.value
  if (props.credits && !props.credits.hasCredits)
    return t('accounts.quota.noPointsAvailableYet')
  return t('usage.balanceUnavailable')
})
const hasBalance = computed(() => !props.credits?.unlimited && balanceDisplay.value !== null)
</script>

<template>
  <div class="flex min-w-0 flex-wrap items-baseline justify-between gap-x-4 gap-y-1 px-4 pb-4" :aria-label="t('accounts.quota.quotaPoints')">
    <span class="text-cp-sm font-heavy text-cp-text">{{ t('accounts.quota.quotaPoints') }}</span>
    <span class="flex min-w-0 items-baseline gap-1.5">
      <strong
        class="min-w-0 text-right text-cp-sm font-heavy wrap-anywhere"
        :class="hasBalance ? 'font-mono tabular-nums text-cp-text' : 'text-cp-text-secondary'"
      >
        {{ value }}
      </strong>
      <span v-if="hasBalance" class="shrink-0 text-cp-xs text-cp-text-tertiary">{{ t('accounts.quota.point') }}</span>
    </span>
  </div>
</template>
