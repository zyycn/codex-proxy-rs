<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountRequestTimeline from './account-usage-window/AccountRequestTimeline.vue'
import { quotaWindowDetails, quotaWindowLabel, resolveAccountUsageWindowPresentation } from './account-usage-window/presenter'

const props = withDefaults(defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
  showConsumption?: boolean
}>(), { showConsumption: true })
const { t } = useI18n({ useScope: 'global' })
const detailHeading = computed(() => props.label)
const detailTitle = computed(() => props.label ?? t('accounts.quota.quotaDetails'))
const detailItems = computed(() =>
  props.windows.map((window) => {
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
      code: quotaWindowCode(window.windowSeconds, window.role),
      label: quotaWindowLabel(window, false),
      usedPercent: window.usedPercent,
      usedPercentDisplay: window.usedPercentDisplay,
      percentTextClass: view.quota.percentTextClass,
      barClass: view.quota.barClass,
      barStyle: view.quota.barStyle,
    }
  }),
)
function quotaWindowCode(windowSeconds: number | null, role: AccountQuotaWindow['role']) {
  if (typeof windowSeconds === 'number' && Number.isFinite(windowSeconds)) {
    const daySeconds = 24 * 60 * 60
    const hourSeconds = 60 * 60

    if (windowSeconds >= daySeconds && windowSeconds % daySeconds === 0)
      return `${windowSeconds / daySeconds}D`
    if (windowSeconds >= hourSeconds && windowSeconds % hourSeconds === 0)
      return `${windowSeconds / hourSeconds}H`
  }

  switch (role) {
    case 'primary':
      return 'P'
    case 'secondary':
      return 'S'
    case 'monthly':
      return 'M'
    default:
      return 'Q'
  }
}
</script>

<template>
  <section class="w-84 max-w-[calc(100vw-2rem)] overflow-hidden rounded-cp-lg" role="dialog" :aria-label="t('accounts.quota.details', { detailTitle })">
    <header v-if="detailHeading" class="bg-cp-popover-header-bg px-3 py-2.5">
      <h3 class="m-0 truncate text-cp leading-5 font-heavy text-cp-text">
        {{ detailHeading }}
      </h3>
    </header>

    <div class="grid gap-4" :class="detailHeading ? 'px-3 pt-3 pb-3' : 'px-4 pt-4 pb-3'">
      <article v-for="item in detailItems" :key="item.key">
        <div class="grid grid-cols-[32px_minmax(0,1fr)_auto] items-center gap-2.5">
          <span
            class="grid h-5 w-8 place-items-center rounded-sm bg-cp-blue-container-strong font-mono text-[9px] leading-none font-heavy tracking-[0.04em] whitespace-nowrap text-cp-blue-on-container"
          >
            {{ item.code }}
          </span>
          <h4 class="m-0 min-w-0 truncate text-cp-sm leading-4 font-heavy text-cp-text-secondary">
            {{ item.local ? item.localLabel : item.label }}
          </h4>
          <strong
            class="font-mono text-cp-xs leading-none font-heavy tabular-nums"
            :class="item.local ? 'text-cp-text' : item.percentTextClass"
          >
            {{ item.local ? t('accounts.quota.requestCount', { requestDisplay: item.requestDisplay }) : item.usedPercentDisplay }}
          </strong>
        </div>

        <AccountRequestTimeline
          v-if="item.local"
          class="mt-2 h-1 w-full"
          :bars="item.requestBars"
          :label="item.timelineTitle"
        />
        <template v-else>
          <div
            class="mt-2 h-1 w-full overflow-hidden rounded-full bg-cp-border-secondary"
            role="progressbar"
            :aria-label="item.label"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="item.stale ? undefined : item.usedPercent ?? undefined"
            :aria-valuetext="item.stale ? `${item.usedPercentDisplay} · ${item.observationDisplay}` : item.usedPercentDisplay"
          >
            <div
              class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
              :class="item.barClass"
              :style="item.barStyle"
            />
          </div>
        </template>

        <p v-if="item.observationDisplay" class="mt-2 text-[10px] leading-4 text-cp-text-tertiary">
          {{ item.observationDisplay }}
        </p>

        <dl class="mt-2.5 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[10px] leading-4">
          <template v-if="item.local">
            <dt class="font-emphasis text-cp-text-tertiary">
              {{ t('accounts.quota.statisticalMethod') }}
            </dt>
            <dd class="m-0 text-right font-mono font-heavy tabular-nums text-cp-text">
              {{ t('accounts.quota.rollingWindow') }}
            </dd>
            <dt class="font-emphasis text-cp-text-tertiary">
              {{ t('accounts.quota.statisticalRange') }}
            </dt>
            <dd class="m-0 truncate text-right font-mono font-emphasis tabular-nums text-cp-text-secondary">
              {{ item.durationDisplay }}
            </dd>
          </template>
          <template v-else>
            <template v-if="showConsumption && item.consumptionDisplay">
              <dt class="font-emphasis text-cp-text-tertiary">
                {{ t('accounts.quota.windowConsumption') }}
              </dt>
              <dd class="m-0 text-right font-mono font-heavy tabular-nums text-cp-text">
                {{ item.consumptionDisplay }}
              </dd>
            </template>
            <dt class="font-emphasis text-cp-text-tertiary">
              {{ t('accounts.quota.resetTime') }}
            </dt>
            <dd class="m-0 truncate text-right font-mono font-emphasis tabular-nums text-cp-text-secondary">
              {{ item.resetAtDisplay }}
            </dd>
          </template>
        </dl>
      </article>
    </div>
  </section>
</template>
