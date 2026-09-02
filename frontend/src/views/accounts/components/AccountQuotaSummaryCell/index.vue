<script setup lang="ts">
import type { AccountRow } from '../../constants'

import { computed } from 'vue'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import AccountUsageWindow from '../AccountUsageWindow/index.vue'
import { quotaWindowPresentation } from '../AccountUsageWindow/presenter'
import AccountQuotaSummaryEntry from './Entry.vue'

const props = defineProps<{
  account: AccountRow
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleQuotaWindows.value))
const hasUsage = computed(() => (props.account.usage.requestCount ?? 0) > 0)
const highestUsageWindow = computed(() => visibleQuotaWindows.value.reduce((highest, window) => {
  if (typeof window.usedPercent !== 'number')
    return highest
  if (!highest || typeof highest.usedPercent !== 'number' || window.usedPercent > highest.usedPercent)
    return window
  return highest
}, undefined as (typeof visibleQuotaWindows.value)[number] | undefined))
const highestUsageDisplay = computed(() => highestUsageWindow.value?.usedPercentDisplay ?? '—')
const highestUsageTextClass = computed(() => highestUsageWindow.value
  ? quotaWindowPresentation(highestUsageWindow.value, '2px').percentTextClass
  : 'text-cp-text-quaternary')
const highestUsageEntry = computed(() => {
  const highestWindow = highestUsageWindow.value
  if (!highestWindow)
    return summaryEntries.value[0]

  return summaryEntries.value.find(entry =>
    entry.windows.some(window => window.key === highestWindow.key),
  ) ?? summaryEntries.value[0]
})
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1.5 py-1.5">
    <template v-if="summaryEntries.length > 0">
      <div
        v-if="hasUsage"
        class="flex min-w-0 items-baseline justify-between gap-2 leading-none"
      >
        <span
          class="flex min-w-0 items-baseline gap-1 font-mono tabular-nums"
          title="当前额度窗口总 Token"
        >
          <strong class="truncate text-cp-xs font-heavy text-cp-text">
            {{ account.usage.totalTokensDisplay }}
          </strong>
          <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-cp-text-quaternary">
            Tokens
          </span>
        </span>
        <span
          class="flex shrink-0 items-baseline gap-1 text-[9px] font-emphasis text-cp-text-quaternary"
          title="所有额度窗口中的最高已用比例"
        >
          <span>最高占用</span>
          <strong class="font-mono font-heavy tabular-nums" :class="highestUsageTextClass">
            {{ highestUsageDisplay }}
          </strong>
        </span>
      </div>

      <div v-if="highestUsageEntry" class="flex min-w-0 items-end gap-2">
        <div class="min-w-0 flex-1">
          <AccountQuotaSummaryEntry
            :label="highestUsageEntry.label"
            :windows="highestUsageEntry.windows"
            :show-percentage="false"
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
