<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api/index'
import type { AccountUsageWindowVariant } from '@/components/account/account-usage-window/presenter'
import { computed } from 'vue'
import AccountRequestTimeline from '@/components/account/account-usage-window/AccountRequestTimeline.vue'
import { resolveAccountUsageWindowPresentation } from '@/components/account/account-usage-window/presenter'

const props = withDefaults(
  defineProps<{
    window?: AccountQuotaWindow
    variant?: AccountUsageWindowVariant
    showLocalValue?: boolean
    showPercentage?: boolean
    showNativeTooltip?: boolean
  }>(),
  {
    variant: 'detail',
    showLocalValue: true,
    showPercentage: true,
    showNativeTooltip: true,
  },
)

const view = computed(() => resolveAccountUsageWindowPresentation({
  window: props.window,
  variant: props.variant,
  showLocalValue: props.showLocalValue,
}))
</script>

<template>
  <div :class="view.classes.root">
    <template v-if="view.mode === 'quota' && window">
      <div :class="view.classes.header">
        <span
          class="min-w-0"
          :class="view.classes.label"
          :title="showNativeTooltip ? view.labelTooltip : undefined"
        >
          {{ window.labelDisplay }}
        </span>
        <span
          v-if="view.compactValues"
          class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums"
        >
          <span
            v-if="view.quota.localUsageVisible"
            class="text-right text-cp-text-quaternary"
            :class="view.classes.value"
            :title="showNativeTooltip ? `窗口消耗：${view.quota.localUsageDisplay}` : undefined"
          >
            {{ view.quota.localUsageDisplay }}
          </span>
          <span
            v-if="showPercentage && view.quota.valueVisible"
            class="text-right"
            :class="[view.classes.value, view.quota.percentTextClass]"
            :title="showNativeTooltip ? `额度已使用：${window.usedPercentDisplay}` : undefined"
          >
            {{ window.usedPercentDisplay }}
          </span>
        </span>
        <span
          v-else
          class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums"
        >
          <span
            v-if="view.quota.localUsageVisible"
            class="text-cp-text-quaternary"
            :class="view.classes.value"
          >
            {{ view.quota.localUsageDisplay }}
          </span>
          <span
            v-if="showPercentage && view.quota.valueVisible"
            :class="[view.classes.value, view.quota.percentTextClass]"
          >
            {{ window.usedPercentDisplay }}
          </span>
        </span>
      </div>
      <div
        class="col-span-full"
        :class="[view.classes.track, view.classes.trackOffset]"
        role="progressbar"
        :aria-label="window.labelDisplay"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-valuenow="window.usedPercent ?? undefined"
      >
        <div
          class="h-full rounded-full transition-[width,background-color] duration-200"
          :class="view.quota.barClass"
          :style="view.quota.barStyle"
        />
      </div>
      <div
        v-if="view.quota.resetVisible"
        class="mt-3 text-cp-sm font-emphasis text-cp-text-secondary"
      >
        重置时间: {{ window.resetAtDisplay }}
      </div>
    </template>

    <template v-else-if="view.mode === 'local'">
      <div :class="view.classes.header">
        <span class="min-w-0" :class="view.classes.label">
          {{ view.local.label }}
        </span>
        <strong
          v-if="view.local.requestValueVisible"
          class="shrink-0 font-mono tabular-nums text-cp-text"
          :class="view.classes.value"
        >
          {{ view.local.requestDisplay }}
        </strong>
      </div>
      <AccountRequestTimeline
        class="col-span-full"
        :bars="view.local.requestBars"
        :label="view.local.timelineTitle"
        :show-native-tooltip="showNativeTooltip"
        :class="[view.classes.trackShape, view.classes.trackOffset]"
      />
    </template>

    <div v-else :class="view.classes.header">
      <span class="min-w-0 text-cp-text-secondary">额度待观测</span>
    </div>
  </div>
</template>
