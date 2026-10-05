<script setup lang="ts">
import type { Account } from '@/api'

import { computed } from 'vue'
import AccountUsageWindow from '@/components/account/account-usage-window/index.vue'
import { quotaWindowPresentation } from '@/components/account/account-usage-window/presenter'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import AccountCapacityIndicator from '../AccountCapacityIndicator.vue'
import AccountQuotaSummaryEntry from './Entry.vue'
import { recentlyUsedQuotaEntry, representativeQuotaWindow } from './presenter'

const props = defineProps<{
  account: Account
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleQuotaWindows.value))
const hasUsage = computed(() => (props.account.usage.requestCount ?? 0) > 0)
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(
  summaryEntries.value,
  props.account.usage.models,
))
const currentUsageWindow = computed(() => representativeQuotaWindow(recentUsageEntry.value))
const currentUsageDisplay = computed(() => currentUsageWindow.value?.usedPercentDisplay ?? '—')
const currentUsageTextClass = computed(() => currentUsageWindow.value
  ? quotaWindowPresentation(currentUsageWindow.value, '2px').percentTextClass
  : 'text-cp-text-quaternary')
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1.5 py-1.5">
    <template v-if="account.authenticationKind === 'api_key'">
      <span
        class="flex min-w-0 items-baseline gap-1 font-mono leading-none tabular-nums"
        title="本地累计总 Token"
      >
        <strong class="truncate text-cp-xs font-heavy text-cp-text">{{ account.usage.totalTokensDisplay }}</strong>
        <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-cp-text-quaternary">Tokens</span>
      </span>
      <div class="grid min-w-0 gap-1.5">
        <div class="flex min-w-0 items-start justify-between gap-2 leading-3">
          <span class="truncate text-[10px] font-bold text-cp-text-quaternary">{{ account.usage.windowLabelDisplay }}</span>
          <AccountCapacityIndicator :capacity="account.capacity" />
        </div>
        <div class="h-1 w-full rounded-full bg-cp-success" title="上游额度未提供，绿色条不表示剩余额度" aria-hidden="true" />
      </div>
    </template>
    <template v-else-if="summaryEntries.length > 0">
      <div
        v-if="hasUsage"
        class="flex min-w-0 items-baseline justify-between gap-2 leading-none"
      >
        <span
          class="flex min-w-0 items-baseline gap-1 font-mono tabular-nums"
          :title="`${account.usage.windowLabelDisplay}总 Token`"
        >
          <strong class="truncate text-cp-xs font-heavy text-cp-text">
            {{ account.usage.totalTokensDisplay }}
          </strong>
          <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-cp-text-quaternary">
            Tokens
          </span>
        </span>
        <span
          v-if="currentUsageWindow"
          class="flex shrink-0 items-baseline gap-1 text-[9px] font-emphasis text-cp-text-quaternary"
          title="最近使用额度的当前已用比例"
        >
          <span>使用率</span>
          <strong class="font-mono font-heavy tabular-nums" :class="currentUsageTextClass">
            {{ currentUsageDisplay }}
          </strong>
        </span>
      </div>

      <div v-if="recentUsageEntry" class="flex min-w-0 items-end gap-2">
        <!-- 共用列轨道为容量留位，进度条仍铺满整行，容量标记独立于额度弹层 -->
        <div class="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2">
          <AccountQuotaSummaryEntry
            class="col-span-full col-start-1 row-start-1"
            :label="recentUsageEntry.label"
            :windows="recentUsageEntry.windows"
            :show-percentage="false"
          />
          <AccountCapacityIndicator
            :capacity="account.capacity"
            class="z-1 col-start-2 row-start-1 self-start"
          />
        </div>
        <span
          v-if="additionalEntryCount > 0"
          class="grid h-5 min-w-5 shrink-0 place-items-center rounded-cp bg-cp-fill-quaternary px-1.5 font-mono text-[9px] font-heavy tabular-nums text-cp-text-tertiary"
          :title="`另有 ${additionalEntryCount} 个额度组，可展开账号查看`"
        >
          +{{ additionalEntryCount }}
        </span>
      </div>
    </template>
    <AccountUsageWindow v-else variant="compact" />
  </div>
</template>
