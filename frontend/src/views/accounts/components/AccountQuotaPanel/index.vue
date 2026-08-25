<script setup lang="ts">
import type { AccountRow } from '../../constants'
import { ChartNoAxesColumnIncreasing, RefreshCw } from '@lucide/vue'

import { computed, shallowRef } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'
import AccountPlanBadge from '../AccountPlanBadge.vue'
import AccountUsageStatisticsModal from '../AccountUsageStatisticsModal/index.vue'
import AccountQuotaPanelEntry from './Entry.vue'
import AccountResetCredits from './ResetCredits.vue'

const props = defineProps<{
  account: AccountRow
  refreshing: boolean
}>()

const emit = defineEmits<{
  refreshQuota: [accountId: string]
  accountUpdated: [account: AccountRow]
}>()

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
const statisticsOpen = shallowRef(false)
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary">
    <div class="mb-3 flex shrink-0 items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          账号额度
        </h3>
        <p
          class="m-0 mt-1 flex min-w-0 items-center gap-1.5 text-cp-xs font-emphasis text-cp-text-secondary"
        >
          <span>{{ account.provider === 'xai' ? 'xAI 用量窗口' : 'Codex 额度' }}</span>
          <template v-if="account.provider === 'openai'">
            <span>·</span>
            <AccountPlanBadge :plan-type="account.planType" size="sm" />
          </template>
          <span>·</span>
          <span>最近刷新: {{ account.quota.refreshedAtDisplay }}</span>
        </p>
      </div>
      <div class="flex shrink-0 items-center gap-0.5">
        <BaseIconButton
          v-if="account.provider === 'openai'"
          label="查看官方用量统计"
          size="sm"
          variant="ghost"
          :pressed="statisticsOpen"
          @click="statisticsOpen = true"
        >
          <ChartNoAxesColumnIncreasing class="size-3.5" />
        </BaseIconButton>
        <AccountResetCredits
          v-if="account.provider === 'openai'"
          :account-id="account.id"
          @account-updated="emit('accountUpdated', $event)"
        />
        <BaseIconButton
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

    <div class="grid max-h-44 min-h-0 gap-3 overflow-y-auto pr-1">
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

  <AccountUsageStatisticsModal
    v-if="account.provider === 'openai'"
    v-model="statisticsOpen"
    :account="account"
  />
</template>
