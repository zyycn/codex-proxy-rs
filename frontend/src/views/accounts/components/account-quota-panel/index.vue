<script setup lang="ts">
import type { Account } from '@/api'

import { ZEmpty, ZIconButton } from '@codex-proxy/ui'

import { ChartNoAxesCombined, RefreshCw, UserRound } from '@lucide/vue'
import { computed, shallowRef, toRef, watch } from 'vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { formatProviderLabel } from '@/utils/providers'
import { useAccountBalance } from '../../composables/useAccountBalance'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'
import AccountProfileModal from '../account-profile-modal/index.vue'
import AccountQuotaForecastModal from '../account-quota-forecast-modal/index.vue'
import AccountBalancePanel from '../AccountBalancePanel.vue'
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
  balanceUpdated: []
}>()
const { balance, loading: balanceLoading, error: balanceError, load: loadBalance } = useAccountBalance(toRef(props, 'account'), () => emit('balanceUpdated'))

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
const profileOpen = shallowRef(false)
const forecastOpen = shallowRef(false)
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const hasPersonalInfo = computed(() => props.account.capabilities.profile || props.account.capabilities.subscription)
const hasBalanceWindows = computed(() => Boolean(balance.value && quotaEntries.value.length > 0))
const hasActions = computed(() => hasPersonalInfo.value || hasForecast.value || hasBalanceWindows.value || props.account.capabilities.quotaRefresh || props.account.capabilities.resetCredits)
watch(hasPersonalInfo, (available) => {
  if (!available)
    profileOpen.value = false
})
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary">
    <div class="mb-3 flex shrink-0 items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          {{ balance ? '上游余额与额度' : '账号额度' }}
        </h3>
        <p
          v-if="account.capabilities.quota || quotaEntries.length > 0"
          class="m-0 mt-1 flex min-w-0 items-center gap-1.5 text-cp-xs font-emphasis text-cp-text-secondary"
        >
          <template v-if="account.authenticationKind !== 'api_key'">
            <span>{{ account.provider === 'openai' ? 'Codex' : formatProviderLabel(account.provider) }} 额度</span>
            <template v-if="account.planType">
              <span>·</span>
              <AccountPlanBadge :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
            </template>
            <span>·</span>
          </template>
          <span>最近刷新: {{ account.quota.refreshedAtDisplay }}</span>
        </p>
      </div>
      <div v-if="hasActions" class="flex shrink-0 items-center gap-0.5 [&_svg]:size-3.5 [&_svg]:stroke-2">
        <ZIconButton
          v-if="hasPersonalInfo"
          aria-label="查看个人信息"
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
          aria-label="预测周/月额度"
          size="small"
          aria-haspopup="dialog"
          :pressed="forecastOpen"
          @click="forecastOpen = true"
        >
          <ChartNoAxesCombined class="size-3.5" />
        </ZIconButton>
        <ZIconButton
          v-if="account.capabilities.quotaRefresh || hasBalanceWindows"
          size="small"
          aria-label="刷新额度"
          :loading="refreshing || balanceLoading"
          :disabled="refreshing || balanceLoading"
          @click="hasBalanceWindows ? void loadBalance(true) : emit('refreshQuota', account.id)"
        >
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </ZIconButton>
      </div>
    </div>

    <p v-if="hasBalanceWindows && balanceError" role="alert" class="m-0 mb-3 text-cp-sm text-cp-error">{{ balanceError }}</p>
    <AccountBalancePanel v-if="balance && !hasBalanceWindows" :balance="balance" :loading="balanceLoading" :error="balanceError" @refresh="void loadBalance(true)" />
    <div v-else-if="!account.capabilities.quota && quotaEntries.length === 0" class="grid flex-1 place-items-center">
      <ZEmpty title="暂不支持查询上游额度" surface="none" />
    </div>
    <div v-else class="grid min-h-0 gap-3">
      <AccountQuotaPanelEntry
        v-for="entry in quotaEntries"
        :key="entry.key"
        :label="entry.label"
        :windows="entry.windows"
      />
      <p v-if="quotaEntries.length === 0" class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
        额度待观测
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
