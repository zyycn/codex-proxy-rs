<script setup lang="ts">
import type { Account } from '@/api'

import { ZEmpty, ZIconButton } from '@codex-proxy/ui'

import { ChartNoAxesCombined, RefreshCw, UserRound } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { formatProviderLabel } from '@/utils/providers'
import { formatRelativeTime } from '@/utils/relative-time'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'
import AccountProfileModal from '../account-profile-modal/index.vue'
import AccountQuotaForecastModal from '../account-quota-forecast-modal/index.vue'
import AccountQuotaPanelEntry from './Entry.vue'
import AccountResetCredits from './ResetCredits.vue'

const props = defineProps<{
  account: Account
  refreshing: boolean
}>()

const emit = defineEmits<{
  accountUpdated: [account: Account]
  refreshQuota: [accountId: string]
  quotaReset: [accountId: string]
}>()

const { t } = useI18n({ useScope: 'global' })

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
const profileOpen = shallowRef(false)
const forecastOpen = shallowRef(false)
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const hasPersonalInfo = computed(() => props.account.capabilities.profile || props.account.capabilities.subscription)
const hasActions = computed(() => hasPersonalInfo.value || hasForecast.value || props.account.capabilities.quotaRefresh || props.account.capabilities.resetCredits)
const lastRefreshed = computed(() => t('accounts.quota.lastRefreshed', {
  refreshedAtDisplay: formatRelativeTime(props.account.quota.refreshedAtRelative),
}))
watch(hasPersonalInfo, (available) => {
  if (!available)
    profileOpen.value = false
})
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary">
    <div class="mb-3 grid shrink-0 grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1">
      <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
        {{ t('accounts.quota.accountLimit') }}
      </h3>
      <p
        v-if="account.capabilities.quota || quotaEntries.length > 0"
        class="col-span-2 row-start-2 m-0 flex min-w-0 items-center gap-1.5 text-cp-xs font-emphasis text-cp-text-secondary"
      >
        <span class="shrink-0 whitespace-nowrap">{{ t('accounts.quota.quota', { provider: formatProviderLabel(account.provider) }) }}</span>
        <template v-if="account.planType">
          <span class="shrink-0">·</span>
          <AccountPlanBadge :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" class="shrink-0" />
        </template>
        <span class="shrink-0">·</span>
        <span class="min-w-0 truncate" :title="lastRefreshed">{{ lastRefreshed }}</span>
      </p>
      <div v-if="hasActions" class="col-start-2 row-start-1 flex items-center gap-0.5 [&_svg]:size-3.5 [&_svg]:stroke-2">
        <ZIconButton
          v-if="hasPersonalInfo"
          :aria-label="t('accounts.quota.viewPersonalInformation')"
          size="small"
          :pressed="profileOpen"
          @click="profileOpen = true"
        >
          <UserRound class="size-3.5" />
        </ZIconButton>
        <AccountResetCredits
          v-if="account.capabilities.resetCredits"
          :account="account"
          @consumed="emit('quotaReset', $event)"
        />
        <ZIconButton
          v-if="hasForecast"
          :aria-label="t('accounts.quota.forecastWeeklyMonthlyQuota')"
          size="small"
          aria-haspopup="dialog"
          :pressed="forecastOpen"
          @click="forecastOpen = true"
        >
          <ChartNoAxesCombined class="size-3.5" />
        </ZIconButton>
        <ZIconButton
          v-if="account.capabilities.quotaRefresh"
          size="small"
          :aria-label="t('accounts.quotaForecast.refreshQuota')"
          :loading="refreshing"
          :disabled="refreshing"
          @click="emit('refreshQuota', account.id)"
        >
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </ZIconButton>
      </div>
    </div>

    <div v-if="!account.capabilities.quota && quotaEntries.length === 0" class="grid flex-1 place-items-center">
      <ZEmpty :title="t('accounts.quota.queryUnsupported')" surface="none" />
    </div>
    <div v-else class="grid min-h-0 gap-3">
      <AccountQuotaPanelEntry
        v-for="entry in quotaEntries"
        :key="entry.key"
        :label="entry.label"
        :windows="entry.windows"
      />
      <p v-if="quotaEntries.length === 0" class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
        {{ t('accounts.quota.unobserved') }}
      </p>
    </div>
  </section>

  <AccountProfileModal
    v-if="hasPersonalInfo"
    v-model="profileOpen"
    :account="account"
  />
  <AccountQuotaForecastModal
    v-if="hasForecast"
    v-model="forecastOpen"
    :account="account"
    @account-updated="emit('accountUpdated', $event)"
  />
</template>
