<script setup lang="ts">
import type { AccountRow } from '../constants'
import { RefreshCw } from '@lucide/vue'

import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import { orderedPanelQuotaWindows } from '../constants'
import AccountPlanBadge from './AccountPlanBadge.vue'
import AccountQuotaWindow from './AccountQuotaWindow.vue'

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
  <section class="rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control)">
    <div class="mb-3 flex items-center justify-between gap-3">
      <div>
        <h3 class="m-0 text-[14px] font-[760] text-(--cp-text-primary)">
          账号额度
        </h3>
        <p
          class="m-0 mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
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
      <BaseButton
        icon-only
        variant="ghost"
        size="sm"
        title="刷新额度"
        :disabled="refreshing"
        @click="emit('refreshQuota', account.id)"
      >
        <RefreshCw class="size-3.5" :class="refreshing ? 'animate-spin' : undefined" />
      </BaseButton>
    </div>

    <div class="grid gap-3">
      <AccountQuotaWindow v-for="window in quotaWindows" :key="window.key" :window="window" />
      <p v-if="quotaWindows.length === 0" class="m-0 text-[12px] font-[620] text-(--cp-text-muted)">
        暂无 Provider quota 观测
      </p>
    </div>
  </section>
</template>
