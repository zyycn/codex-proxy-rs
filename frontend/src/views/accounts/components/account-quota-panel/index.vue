<script setup lang="ts">
import type { Account } from '@/api'

import { BaseEmpty, BaseIconButton } from '@codex-proxy/ui'

import { ChartNoAxesCombined, RefreshCw, UserRound } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { formatProviderLabel } from '@/utils/providers'
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

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
const profileOpen = shallowRef(false)
const forecastOpen = shallowRef(false)
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const hasPersonalInfo = computed(() => props.account.capabilities.profile || props.account.capabilities.subscription)
const hasActions = computed(() => hasPersonalInfo.value || hasForecast.value || props.account.capabilities.quotaRefresh || props.account.capabilities.resetCredits)
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
          账号额度
        </h3>
        <p
          v-if="account.capabilities.quota || quotaEntries.length > 0"
          class="m-0 mt-1 flex min-w-0 items-center gap-1.5 text-cp-xs font-emphasis text-cp-text-secondary"
        >
          <span>{{ account.provider === 'openai' ? 'Codex' : formatProviderLabel(account.provider) }} 额度</span>
          <template v-if="account.planType">
            <span>·</span>
            <AccountPlanBadge :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
          </template>
          <span>·</span>
          <span>最近刷新: {{ account.quota.refreshedAtDisplay }}</span>
        </p>
      </div>
      <div v-if="hasActions" class="flex shrink-0 items-center gap-0.5 [&_svg]:size-3.5 [&_svg]:stroke-2">
        <BaseIconButton
          v-if="hasPersonalInfo"
          label="查看个人信息"
          size="sm"
          variant="ghost"
          :pressed="profileOpen"
          @click="profileOpen = true"
        >
          <UserRound class="size-3.5" />
        </BaseIconButton>
        <AccountResetCredits
          v-if="account.capabilities.resetCredits"
          :account="account"
          @consumed="emit('quotaReset', $event)"
        />
        <BaseIconButton
          v-if="hasForecast"
          label="预测周/月额度"
          size="sm"
          variant="ghost"
          aria-haspopup="dialog"
          :pressed="forecastOpen"
          @click="forecastOpen = true"
        >
          <ChartNoAxesCombined class="size-3.5" />
        </BaseIconButton>
        <BaseIconButton
          v-if="account.capabilities.quotaRefresh"
          variant="ghost"
          size="sm"
          label="刷新额度"
          :loading="refreshing"
          :disabled="refreshing"
          @click="emit('refreshQuota', account.id)"
        >
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </BaseIconButton>
      </div>
    </div>

    <div v-if="!account.capabilities.quota && quotaEntries.length === 0" class="grid flex-1 place-items-center">
      <BaseEmpty title="暂不支持查询上游额度" surface="none" />
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
