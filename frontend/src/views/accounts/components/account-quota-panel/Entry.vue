<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import AccountRequestTimeline from '@/components/account/account-usage-window/AccountRequestTimeline.vue'
import {
  quotaWindowDetails,
  quotaWindowLabel,
  quotaWindowPresentation,
  resolveAccountUsageWindowPresentation,
} from '@/components/account/account-usage-window/presenter'

const props = defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
}>()

const { t } = useI18n({ useScope: 'global' })

const grouped = computed(() => props.windows.length > 1 && Boolean(props.label))
const items = computed(() => props.windows.map((window) => {
  const presentation = quotaWindowPresentation(window, '4px')
  const view = resolveAccountUsageWindowPresentation({
    window,
    variant: 'detail',
    showLocalValue: true,
  })

  return {
    ...quotaWindowDetails(window),
    key: window.key,
    local: view.mode === 'local',
    localLabel: view.local.label,
    requestDisplay: view.local.requestDisplay,
    requestBars: view.local.requestBars,
    timelineTitle: view.local.timelineTitle,
    durationDisplay: view.local.durationDisplay,
    label: grouped.value
      ? quotaWindowLabel(window, false)
      : quotaWindowLabel(window),
    ariaLabel: quotaWindowLabel(window),
    usedPercent: window.usedPercent,
    percentTextClass: presentation.percentTextClass,
    barClass: presentation.barClass,
    barStyle: presentation.barStyle,
  }
}))
</script>

<template>
  <section class="grid min-w-0 gap-2">
    <h4
      v-if="grouped && label && windows[0]?.limitId !== 'codex'"
      class="m-0 truncate text-cp-xs leading-4 font-heavy text-cp-text-secondary"
      :title="label"
    >
      {{ label }}
    </h4>

    <div
      class="grid min-w-0 gap-3"
      :class="grouped ? 'grid-cols-2' : 'grid-cols-1'"
    >
      <div v-for="item in items" :key="item.key" class="grid min-w-0 gap-1.5">
        <template v-if="item.local">
          <div class="flex min-w-0 items-baseline justify-between gap-2 text-cp-xs leading-3.5">
            <span class="min-w-0 truncate font-bold text-cp-text-quaternary">
              {{ item.localLabel }}
            </span>
            <strong
              class="shrink-0 font-mono font-heavy tabular-nums text-cp-text"
              :title="t('accounts.quota.requestsInThisWindow', { requestDisplay: item.requestDisplay })"
            >
              {{ t('accounts.quota.requestCount', { requestDisplay: item.requestDisplay }) }}
            </strong>
          </div>

          <AccountRequestTimeline
            class="h-1.5 w-full"
            :bars="item.requestBars"
            :label="item.timelineTitle"
          />

          <p class="m-0 flex min-w-0 items-center justify-between gap-2 text-[10px] leading-3.5 text-cp-text-tertiary">
            <span class="shrink-0 font-emphasis">{{ t('accounts.quota.statisticalRange') }}</span>
            <span
              class="min-w-0 truncate font-mono font-emphasis tabular-nums"
              :title="t('accounts.quota.rolling', { duration: item.durationDisplay })"
            >
              {{ t('accounts.quota.rolling', { duration: item.durationDisplay }) }}
            </span>
          </p>
        </template>
        <template v-else>
          <div class="flex min-w-0 items-baseline justify-between gap-2 text-cp-xs leading-3.5">
            <span class="min-w-0 truncate font-bold text-cp-text-quaternary" :title="item.observationDisplay || undefined">
              {{ item.label }}
            </span>
            <span class="flex shrink-0 items-baseline gap-1.5 font-mono font-heavy tabular-nums">
              <template v-if="item.amountUsedDisplay">
                <strong class="text-cp-text-quaternary">
                  {{ item.amountUsedDisplay }}<template v-if="item.usedPercent === null"> / {{ item.quotaValueDisplay }}</template>
                </strong>
                <span v-if="item.localUsageDisplay" class="font-normal text-cp-text-quaternary" aria-hidden="true">·</span>
              </template>
              <strong
                v-if="item.localUsageDisplay"
                class="text-cp-text-quaternary"
                :title="t('accounts.quota.usageInThisWindow', { localUsageDisplay: item.localUsageDisplay })"
              >
                {{ item.localUsageDisplay }}
              </strong>
              <strong
                v-if="!item.amountUsedDisplay || item.usedPercent !== null"
                :class="item.percentTextClass"
                :title="item.usedPercent === null ? undefined : t('accounts.quota.quotaUsed', { rate: item.quotaValueDisplay })"
              >
                {{ item.quotaValueDisplay }}
              </strong>
            </span>
          </div>

          <div
            class="h-1.5 w-full overflow-hidden rounded-full bg-cp-border-secondary"
            role="progressbar"
            :aria-label="item.ariaLabel"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="item.stale ? undefined : item.usedPercent ?? undefined"
            :aria-valuetext="item.stale ? `${item.quotaValueDisplay} · ${item.observationDisplay}` : item.quotaValueDisplay"
          >
            <div
              class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
              :class="item.barClass"
              :style="item.barStyle"
            />
          </div>

          <p class="m-0 flex min-w-0 items-center justify-between gap-2 text-[10px] leading-3.5 text-cp-text-tertiary">
            <span class="shrink-0 font-emphasis">{{ t('common.actions.reset') }}</span>
            <span class="min-w-0 truncate font-mono font-emphasis tabular-nums" :title="item.resetAtDisplay">
              {{ item.resetAtDisplay }}
            </span>
          </p>
        </template>
      </div>
    </div>
  </section>
</template>
