<script setup lang="ts">
import type { Account } from '@/api'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { quotaWindowPresentation } from '@/components/account-usage-window/presenter'
import { groupedAccountQuotaWindows, usageWindowLabels, visibleSummaryQuotaWindows } from '../../constants'
import AccountCapacityIndicator from '../AccountCapacityIndicator.vue'
import AccountQuotaSummaryEntry from './Entry.vue'
import { recentlyUsedQuotaEntry, representativeQuotaWindow } from './presenter'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{ showConcurrency: [] }>()

const { t } = useI18n({ useScope: 'global' })

const localUsageOnly = computed(() => props.account.authenticationKind === 'api_key' && !props.account.upstream?.discovered)

// 互联列表只展示此 Key 的消费限额，池内各账号的百分比不能代表整个上游
const quotaWindows = computed(() => props.account.upstream?.discovered
  ? props.account.quota.windows.filter(window => window.amount)
  : props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => props.account.upstream?.discovered && visibleQuotaWindows.value.length
  ? [
      {
        key: 'upstream-key',
        label: t('accounts.quota.general'),
        windows: visibleQuotaWindows.value,
      },
    ]
  : groupedAccountQuotaWindows(visibleQuotaWindows.value))
const hasTokenUsage = computed(() => (props.account.usage.totalTokens ?? 0) > 0)
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(
  summaryEntries.value,
  props.account.usage.models,
))
const currentUsageWindow = computed(() => representativeQuotaWindow(recentUsageEntry.value))
const currentUsageDisplay = computed(() => currentUsageWindow.value?.usedPercentDisplay ?? '—')
const summaryValue = computed(() => {
  return {
    value: props.account.usage.totalTokensDisplay,
    label: t('common.units.tokens'),
    title: props.account.authenticationKind === 'api_key'
      ? t('accounts.quota.localTokens')
      : t('accounts.quota.totalTokensFor', { windowLabelDisplay: usageWindowLabels.value[props.account.usage.windowKind] }),
  }
})
const currentUsageTextClass = computed(() => currentUsageWindow.value
  ? quotaWindowPresentation(currentUsageWindow.value, '2px').percentTextClass
  : 'text-z-text-quaternary')
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1.5 py-1.5">
    <div
      v-if="localUsageOnly || hasTokenUsage || currentUsageWindow"
      class="flex min-w-0 items-baseline justify-between gap-2 leading-none"
    >
      <span
        v-if="localUsageOnly || hasTokenUsage"
        class="flex min-w-0 items-baseline gap-1 font-mono tabular-nums"
        :title="summaryValue.title"
      >
        <strong class="truncate text-z-xs font-heavy text-z-text">
          {{ summaryValue.value }}
        </strong>
        <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-z-text-quaternary">
          {{ summaryValue.label }}
        </span>
      </span>
      <span
        v-if="currentUsageWindow"
        class="ml-auto flex shrink-0 items-baseline gap-1 text-[9px] font-emphasis text-z-text-quaternary"
        :title="t('accounts.quota.usageRateHelp')"
      >
        <span>{{ t('accounts.quota.usageRate') }}</span>
        <strong class="font-mono font-heavy tabular-nums" :class="currentUsageTextClass">
          {{ currentUsageDisplay }}
        </strong>
      </span>
    </div>

    <div v-if="localUsageOnly" class="grid min-w-0 gap-1.5">
      <div class="flex min-w-0 items-start justify-between gap-2 leading-3">
        <span class="truncate text-[10px] font-bold text-z-text-quaternary">{{ usageWindowLabels[account.usage.windowKind] }}</span>
        <AccountCapacityIndicator :capacity="account.capacity" @show-details="emit('showConcurrency')" />
      </div>
      <div class="h-1 w-full rounded-full bg-z-success" aria-hidden="true" />
    </div>
    <div v-else class="flex min-w-0 items-end gap-2">
      <!-- 共用列轨道为容量留位，进度条仍铺满整行，容量标记独立于额度弹层 -->
      <div class="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2">
        <AccountQuotaSummaryEntry
          class="col-span-full col-start-1 row-start-1"
          :label="recentUsageEntry ? recentUsageEntry.label : t('accounts.upstream.quota')"
          :windows="recentUsageEntry?.windows ?? []"
          :show-percentage="false"
        />
        <AccountCapacityIndicator
          :capacity="account.capacity" class="z-1 col-start-2 row-start-1 self-start"
          @show-details="emit('showConcurrency')"
        />
      </div>
      <span
        v-if="additionalEntryCount > 0"
        class="grid h-5 min-w-5 shrink-0 place-items-center rounded-z bg-z-fill-quaternary px-1.5 font-mono text-[9px] font-heavy tabular-nums text-z-text-tertiary"
        :title="t('accounts.quota.moreGroups', { count: additionalEntryCount })"
      >
        +{{ additionalEntryCount }}
      </span>
    </div>
  </div>
</template>
