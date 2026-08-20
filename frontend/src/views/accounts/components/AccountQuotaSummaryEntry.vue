<script setup lang="ts">
import type { AccountQuotaWindow } from '../constants'

import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { useUiClock } from '@/composables/useUiClock'
import AccountQuotaWindowGroup from './AccountQuotaWindowGroup.vue'
import AccountUsageWindow from './AccountUsageWindow/index.vue'
import { resolveAccountUsageWindowPresentation } from './AccountUsageWindow/presenter'

const props = defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
}>()

const now = useUiClock()
const grouped = computed(() => (
  props.windows.length > 1
  && Boolean(props.label)
))
const detailHeading = computed(() => (
  props.label
))
const detailTitle = computed(() => (
  detailHeading.value
  ?? props.windows[0]?.labelDisplay
  ?? '额度详情'
))
const detailItems = computed(() => props.windows.map((window) => {
  const view = resolveAccountUsageWindowPresentation({
    window,
    variant: 'detail',
    showLocalValue: true,
    now: now.value.getTime(),
  })

  return {
    key: window.key,
    code: quotaWindowCode(window.windowSeconds, window.role),
    label: window.windowLabelDisplay,
    usedPercent: window.usedPercent,
    usedPercentDisplay: window.usedPercentDisplay,
    resetAtDisplay: window.resetAtDisplay,
    localUsageDisplay: view.quota.localUsageVisible
      ? view.quota.localUsageDisplay
      : '—',
    percentTextClass: view.quota.percentTextClass,
    barClass: view.quota.barClass,
    barStyle: view.quota.barStyle,
  }
}))
function quotaWindowCode(
  windowSeconds: number | null,
  role: AccountQuotaWindow['role'],
) {
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
  <BasePopover
    class="w-full"
    trigger="hover-click"
    placement="right"
    :hover-delay="240"
  >
    <template #trigger="{ open }">
      <button
        type="button"
        class="block w-full cursor-help rounded-sm border-0 bg-transparent p-0 text-left outline-none focus-visible:ring-2 focus-visible:ring-cp-info-border"
        :aria-label="`查看${detailTitle}详情`"
        :aria-expanded="open"
        aria-haspopup="dialog"
      >
        <AccountQuotaWindowGroup
          v-if="grouped && label"
          :label="label"
          :windows="windows"
        />
        <AccountUsageWindow
          v-else
          :window="windows[0]"
          variant="compact"
          :show-local-value="true"
          :show-percentage="false"
          :show-native-tooltip="false"
        />
      </button>
    </template>

    <section
      class="w-84 overflow-hidden rounded-cp-overlay"
      role="dialog"
      :aria-label="`${detailTitle}详情`"
    >
      <header
        v-if="detailHeading"
        class="bg-cp-muted p-3"
      >
        <h3
          class="m-0 truncate text-[13px] leading-5 font-heavy text-cp-primary"
          :title="detailHeading"
        >
          {{ detailHeading }}
        </h3>
      </header>

      <div
        class="grid gap-4"
        :class="detailHeading ? 'p-3' : 'p-4'"
      >
        <article
          v-for="item in detailItems"
          :key="item.key"
        >
          <div class="grid grid-cols-[32px_minmax(0,1fr)_auto] items-center gap-2.5">
            <span
              class="grid h-5 w-8 place-items-center rounded-sm bg-cp-muted font-mono text-[9px] leading-none font-heavy tracking-[0.04em] text-cp-info-text"
              aria-hidden="true"
            >
              {{ item.code }}
            </span>
            <h4 class="m-0 min-w-0 truncate text-[12px] leading-4 font-heavy text-cp-secondary">
              {{ item.label }}
            </h4>
            <strong
              class="font-mono text-[11px] leading-none font-heavy tabular-nums"
              :class="item.percentTextClass"
            >
              {{ item.usedPercentDisplay }}
            </strong>
          </div>

          <div
            class="mt-2 h-1 w-full overflow-hidden rounded-full bg-cp-default-border"
            role="progressbar"
            :aria-label="item.label"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="item.usedPercent ?? undefined"
            :aria-valuetext="item.usedPercentDisplay"
          >
            <div
              class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
              :class="item.barClass"
              :style="item.barStyle"
            />
          </div>

          <dl class="mt-2.5 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[10px] leading-4">
            <dt class="font-emphasis text-cp-tertiary">
              窗口消耗
            </dt>
            <dd class="m-0 text-right font-mono font-heavy tabular-nums text-cp-primary">
              {{ item.localUsageDisplay }}
            </dd>
            <dt class="font-emphasis text-cp-tertiary">
              重置时间
            </dt>
            <dd class="m-0 truncate text-right font-mono font-emphasis tabular-nums text-cp-secondary">
              {{ item.resetAtDisplay }}
            </dd>
          </dl>
        </article>
      </div>
    </section>
  </BasePopover>
</template>
