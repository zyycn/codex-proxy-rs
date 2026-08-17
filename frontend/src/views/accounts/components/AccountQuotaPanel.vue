<script setup lang="ts">
import type { AccountRow } from '../constants'
import { RefreshCw } from '@lucide/vue'

import { computed } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { orderedPanelQuotaWindows } from '../constants'
import AccountPlanBadge from './AccountPlanBadge.vue'
import AccountUsageWindow from './AccountUsageWindow/index.vue'

const props = defineProps<{
  account: AccountRow
  refreshing: boolean
}>()

const emit = defineEmits<{
  refreshQuota: [accountId: string]
}>()

const quotaWindows = computed(() => orderedPanelQuotaWindows(props.account.quota.windows))
</script>

<template>
  <section class="rounded-lg bg-cp-surface p-4 shadow-cp-control">
    <div class="mb-3 flex items-center justify-between gap-3">
      <div>
        <h3 class="m-0 text-[14px] font-heavy text-cp-primary">
          账号额度
        </h3>
        <p
          class="m-0 mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[12px] font-emphasis text-cp-secondary"
        >
          <span>{{ account.provider === 'xai' ? 'xAI 用量窗口' : 'Codex 额度' }}</span>
          <template v-if="account.provider === 'openai'">
            <span>·</span>
            <span>套餐:</span>
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

    <div class="grid gap-3">
      <AccountUsageWindow v-for="window in quotaWindows" :key="window.key" :window="window" />
      <AccountUsageWindow v-if="quotaWindows.length === 0" />
    </div>
  </section>
</template>
