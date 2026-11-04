<script setup lang="ts">
import type { Account } from '@/api'

import { ZEmpty, ZIconButton, ZScrollbar } from '@codex-proxy/ui'

import { ChartNoAxesCombined, RefreshCw, UserRound } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { formatRelativeTime } from '@/utils/relative-time'
import { usePoolQuotaWindows } from '../../composables/usePoolQuotaWindows'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'
import AccountProfileModal from '../account-profile-modal/index.vue'
import AccountQuotaForecastModal from '../account-quota-forecast-modal/index.vue'
import AccountQuotaPanelEntry from './Entry.vue'
import AccountResetCredits from './ResetCredits.vue'
import AccountUpstreamPool from './UpstreamPool.vue'

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
const poolWindows = usePoolQuotaWindows(() => props.account)
const hasUpstreamPool = computed(() => props.account.upstream?.discovered && (
  props.account.upstream.pool || props.account.upstream.maxConcurrency != null || props.account.upstream.requestsPerMinute != null
  || poolWindows.value.some(window => window.sourceAccountId && !window.amount)
))

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.upstream?.discovered
    ? props.account.quota.windows.filter(window => window.amount)
    : props.account.quota.windows),
))
const profileOpen = shallowRef(false)
const forecastOpen = shallowRef(false)
const hasPlanBadge = computed(() => Boolean(props.account.planType) || props.account.authenticationKind === 'api_key')
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const hasPersonalInfo = computed(() => props.account.capabilities.profile || props.account.capabilities.subscription)
const canRefresh = computed(() => props.account.capabilities.quotaRefresh || props.account.upstream != null)
const hasActions = computed(() => hasPersonalInfo.value || hasForecast.value || canRefresh.value || props.account.capabilities.resetCredits)
const lastRefreshed = computed(() => {
  const { upstream, quota } = props.account
  if (upstream?.discovered && !upstream.observedAt)
    return undefined
  const refreshedAt = upstream?.discovered ? upstream.observedAtRelative : quota.refreshedAtRelative
  return refreshedAt
    ? t('accounts.quota.lastRefreshed', {
        refreshedAtDisplay: formatRelativeTime(refreshedAt),
      })
    : undefined
})
const refreshTitle = computed(() => props.account.upstream?.stale && props.account.upstream.observedAt
  ? `${props.account.upstream.observedAtDisplay} · ${t('accounts.upstream.stale')}`
  : lastRefreshed.value)
watch(hasPersonalInfo, (available) => {
  if (!available)
    profileOpen.value = false
})
</script>

<template>
  <section class="flex max-h-72 min-h-0 min-w-0 flex-col rounded-lg bg-z-bg-container p-4 shadow-z-tertiary">
    <div class="mb-3 grid shrink-0 grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1">
      <h3 class="m-0 text-z-lg font-heavy text-z-text">
        {{ account.upstream?.discovered ? t('accounts.upstream.quota') : t('accounts.quota.accountLimit') }}
      </h3>
      <p
        v-if="(account.capabilities.quota || quotaEntries.length > 0 || account.upstream?.discovered) && (hasPlanBadge || lastRefreshed)"
        class="col-span-2 row-start-2 m-0 flex min-w-0 items-center gap-1.5 text-z-xs font-emphasis text-z-text-secondary"
      >
        <AccountPlanBadge v-if="hasPlanBadge" :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" class="shrink-0" />
        <span v-if="lastRefreshed" class="min-w-0 truncate" :title="refreshTitle">{{ lastRefreshed }}</span>
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
          v-if="canRefresh"
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

    <div v-if="quotaEntries.length === 0 && !hasUpstreamPool" class="grid flex-1 place-items-center">
      <ZEmpty
        :title="!account.capabilities.quota && !account.upstream?.discovered ? t('accounts.quota.queryUnsupported') : t('accounts.quota.unobserved')"
        surface="none"
      />
    </div>
    <div v-else class="flex min-h-0 flex-1 flex-col gap-3">
      <ZScrollbar v-if="quotaEntries.length" height="100%" class="-mr-3 [--z-scrollbar-track-inset-block-start:0px]">
        <div class="grid gap-3 pr-3">
          <AccountQuotaPanelEntry
            v-for="entry in quotaEntries"
            :key="entry.key"
            :label="entry.label"
            :windows="entry.windows"
          />
        </div>
      </ZScrollbar>
      <AccountUpstreamPool v-if="hasUpstreamPool" class="mt-auto" :upstream="account.upstream" :windows="poolWindows" />
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
