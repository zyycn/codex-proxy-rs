<script setup lang="ts">
import type { AccountQuotaWindow } from '../../constants'

import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { useUiClock } from '@/composables/useUiClock'
import AccountRequestTimeline from '../AccountUsageWindow/AccountRequestTimeline.vue'
import AccountUsageWindow from '../AccountUsageWindow/index.vue'
import { resolveAccountUsageWindowPresentation } from '../AccountUsageWindow/presenter'
import AccountQuotaWindowGroup from './WindowGroup.vue'

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
    local: view.mode === 'local',
    localLabel: view.local.label,
    requestDisplay: view.local.requestDisplay,
    requestBars: view.local.requestBars,
    timelineTitle: view.local.timelineTitle,
    durationDisplay: view.local.durationDisplay,
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
        class="block w-full cursor-pointer rounded-sm border-0 bg-transparent p-0 text-left outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
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
      class="w-84 overflow-hidden rounded-cp-lg"
      role="dialog"
      :aria-label="`${detailTitle}详情`"
    >
      <header
        v-if="detailHeading"
        class="bg-cp-fill-tertiary px-3 py-2.5"
      >
        <h3
          class="m-0 truncate text-cp leading-5 font-heavy text-cp-text"
          :title="detailHeading"
        >
          {{ detailHeading }}
        </h3>
      </header>

      <div
        class="grid gap-4"
        :class="detailHeading ? 'px-3 pt-3 pb-3' : 'px-4 pt-4 pb-3'"
      >
        <article
          v-for="item in detailItems"
          :key="item.key"
        >
          <div class="grid grid-cols-[32px_minmax(0,1fr)_auto] items-center gap-2.5">
            <span
              class="grid h-5 w-8 place-items-center rounded-sm bg-cp-blue-bg-strong font-mono text-[9px] leading-none font-heavy tracking-[0.04em] text-cp-blue-text-on-bg"
              aria-hidden="true"
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
              {{ item.local ? `${item.requestDisplay} 次` : item.usedPercentDisplay }}
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
              :aria-valuenow="item.usedPercent ?? undefined"
              :aria-valuetext="item.usedPercentDisplay"
            >
              <div
                class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
                :class="item.barClass"
                :style="item.barStyle"
              />
            </div>
          </template>

          <dl class="mt-2.5 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[10px] leading-4">
            <template v-if="item.local">
              <dt class="font-emphasis text-cp-text-tertiary">
                统计方式
              </dt>
              <dd class="m-0 text-right font-mono font-heavy tabular-nums text-cp-text">
                滚动窗口
              </dd>
              <dt class="font-emphasis text-cp-text-tertiary">
                统计范围
              </dt>
              <dd class="m-0 truncate text-right font-mono font-emphasis tabular-nums text-cp-text-secondary">
                {{ item.durationDisplay }}
              </dd>
            </template>
            <template v-else>
              <dt class="font-emphasis text-cp-text-tertiary">
                窗口消耗
              </dt>
              <dd class="m-0 text-right font-mono font-heavy tabular-nums text-cp-text">
                {{ item.localUsageDisplay }}
              </dd>
              <dt class="font-emphasis text-cp-text-tertiary">
                重置时间
              </dt>
              <dd class="m-0 truncate text-right font-mono font-emphasis tabular-nums text-cp-text-secondary">
                {{ item.resetAtDisplay }}
              </dd>
            </template>
          </dl>
        </article>
      </div>
    </section>
  </BasePopover>
</template>
