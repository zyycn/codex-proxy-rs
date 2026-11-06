<script setup lang="ts">
import type { Account } from '@/api'

import { ZSwitch } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { batchUpdateAccounts } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { numberLocale } from '@/utils/format'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{ accountUpdated: [account: Account] }>()
const { loading, run } = useAsyncAction()
const pending = shallowRef<boolean | null>(null)
const credits = computed(() => props.account.quota.credits)

async function toggleCredits(enabled: boolean) {
  const accountId = props.account.id
  await run(async () => {
    pending.value = enabled
    await batchUpdateAccounts({
      accountIds: [accountId],
      autoUseQuotaCredits: enabled,
    })
    if (props.account.id === accountId)
      emit('accountUpdated', { ...props.account, autoUseQuotaCredits: enabled })
  })
  pending.value = null
}

const { t } = useI18n({ useScope: 'global' })

const balanceDisplay = computed(() => {
  const balance = credits.value?.balance?.trim()
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
  if (credits.value?.unlimited)
    return t('accounts.quota.unlimited')
  if (balanceDisplay.value !== null)
    return balanceDisplay.value
  if (credits.value && !credits.value.hasCredits)
    return t('accounts.quota.noPointsAvailableYet')
  return t('usage.balanceUnavailable')
})
const hasBalance = computed(() => !credits.value?.unlimited && balanceDisplay.value !== null)
</script>

<template>
  <section class="grid gap-3 rounded-z bg-z-fill-quaternary px-4 py-3.5" :aria-label="t('accounts.quota.quotaPoints')">
    <div class="flex min-w-0 flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
      <span class="text-z-sm font-heavy text-z-text">{{ t('accounts.quota.quotaPoints') }}</span>
      <span class="flex min-w-0 items-baseline gap-1.5">
        <strong
          class="min-w-0 text-right text-z-sm font-heavy wrap-anywhere"
          :class="hasBalance ? 'font-mono tabular-nums text-z-text' : 'text-z-text-secondary'"
        >
          {{ value }}
        </strong>
        <span v-if="hasBalance" class="shrink-0 text-z-xs text-z-text-tertiary">{{ t('accounts.quota.point') }}</span>
      </span>
    </div>
    <div v-if="account.capabilities.quotaCredits" class="flex items-center justify-between gap-4">
      <span class="min-w-0">
        <span class="block text-z-sm font-heavy text-z-text">{{ t('accounts.quota.autoUsePoints') }}</span>
        <span class="mt-1 block text-z-xs text-z-text-secondary">{{ t('accounts.quota.autoUsePointsHelp') }}</span>
      </span>
      <ZSwitch
        :model-value="pending ?? account.autoUseQuotaCredits"
        :aria-label="t('accounts.quota.autoUsePoints')"
        :disabled="loading"
        @update:model-value="toggleCredits"
      />
    </div>
  </section>
</template>
