<script setup lang="ts">
import type { AccountRow } from '../constants'
import { RefreshCw } from '@lucide/vue'

import { computed } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../constants'
import AccountPlanBadge from './AccountPlanBadge.vue'
import AccountQuotaPanelEntry from './AccountQuotaPanelEntry.vue'

const props = defineProps<{
  account: AccountRow
  refreshing: boolean
}>()

const emit = defineEmits<{
  refreshQuota: [accountId: string]
}>()

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg bg-cp-surface p-4 shadow-cp-control">
    <div class="mb-3 flex shrink-0 items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="m-0 text-[14px] font-heavy text-cp-primary">
          账号额度
        </h3>
        <p
          class="m-0 mt-1 flex min-w-0 items-center gap-1.5 text-[11px] font-emphasis text-cp-secondary"
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

    <div class="grid max-h-48 min-h-0 gap-3 overflow-y-auto pr-1">
      <AccountQuotaPanelEntry
        v-for="entry in quotaEntries"
        :key="entry.key"
        :label="entry.label"
        :windows="entry.windows"
      />
      <p v-if="quotaEntries.length === 0" class="m-0 text-[12px] font-emphasis text-cp-secondary">
        额度待观测
      </p>
    </div>
  </section>
</template>
