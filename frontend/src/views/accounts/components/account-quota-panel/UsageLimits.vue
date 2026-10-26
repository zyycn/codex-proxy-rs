<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { quotaWindowLabel, quotaWindowPresentation } from '@/components/account/account-usage-window/presenter'
import { orderedPanelQuotaWindows } from '../../constants'

const props = defineProps<{
  windows: AccountQuotaWindow[]
}>()

const { t } = useI18n({ useScope: 'global' })

const items = computed(() => orderedPanelQuotaWindows(props.windows)
  .filter(window => window.limitId === 'codex' || !window.limitId)
  .map((window) => {
    const remaining = window.usedPercent !== null && Number.isFinite(window.usedPercent)
      ? Math.round((100 - Math.min(100, Math.max(0, window.usedPercent))) * 10) / 10
      : null
    const presentation = quotaWindowPresentation(window, '0')

    return {
      key: window.key,
      label: quotaWindowLabel(window),
      resetAtDisplay: window.resetAtDisplay,
      remaining,
      remainingDisplay: remaining === null
        ? t('accounts.quota.awaitingData')
        : t('accounts.quota.remaining', { remaining }),
      barClass: presentation.barClass,
      percentTextClass: presentation.percentTextClass,
      barStyle: { width: `${remaining ?? 0}%` },
    }
  }))
</script>

<template>
  <section class="grid gap-4 p-4" :aria-label="t('accounts.quota.usageLimit')">
    <div
      v-for="item in items"
      :key="item.key"
      class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:items-center sm:gap-5"
    >
      <div class="min-w-0">
        <h3 class="m-0 text-cp-sm font-heavy text-cp-text">
          {{ item.label }}
        </h3>
        <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
          {{ t('accounts.quota.reset') }}<span class="font-mono tabular-nums">{{ item.resetAtDisplay }}</span>
        </p>
      </div>
      <div class="flex min-w-0 items-center gap-3">
        <div
          class="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-cp-border-secondary"
          role="progressbar"
          :aria-label="t('accounts.quota.remainingQuota', { label: item.label })"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="item.remaining ?? undefined"
          :aria-valuetext="item.remainingDisplay"
        >
          <div
            class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
            :class="item.barClass"
            :style="item.barStyle"
          />
        </div>
        <span class="shrink-0 text-cp-xs font-heavy tabular-nums" :class="item.percentTextClass">
          {{ item.remainingDisplay }}
        </span>
      </div>
    </div>
    <p v-if="items.length === 0" class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
      {{ t('accounts.quota.unobserved') }}
    </p>
  </section>
</template>
