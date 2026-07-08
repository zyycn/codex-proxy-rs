<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'
import { clamp } from 'es-toolkit'

import BaseButton from '@/components/base/BaseButton.vue'
import AccountPlanBadge from './AccountPlanBadge.vue'

const props = defineProps<{
  account: any
  refreshing: boolean
}>()

const emit = defineEmits<{
  refreshQuota: [accountId: string]
}>()

function quotaWindows(group: string) {
  return (props.account.quota.windows as any[]).filter((window) => window.group === group)
}

function quotaWindowPercent(window?: any) {
  return clamp(window?.usedPercent ?? 0, 0, 100)
}

function quotaWindowBarStyle(window?: any) {
  const percent = quotaWindowPercent(window)
  return {
    width: `${percent}%`,
    minWidth: percent > 0 ? '8px' : '0',
  }
}

function quotaWindowBarClass(window?: any) {
  if (window?.usedPercent === null || window?.usedPercent === undefined) {
    return 'bg-(--cp-default-border-hover)'
  }
  if (window.usedPercent >= 95) {
    return 'bg-(--cp-danger)'
  }
  if (window.usedPercent >= 80) {
    return 'bg-(--cp-warning)'
  }
  return 'bg-(--cp-success)'
}

function quotaWindowPercentTextClass(window?: any) {
  if (window?.usedPercent === null || window?.usedPercent === undefined) {
    return 'text-(--cp-text-muted)'
  }
  if (window.usedPercent >= 95) {
    return 'text-(--cp-danger-text)'
  }
  if (window.usedPercent >= 80) {
    return 'text-(--cp-warning-text)'
  }
  return 'text-(--cp-success-text)'
}

function quotaWindowLocalUsageDisplay(window?: any) {
  const display = window?.localUsage?.totalTokensDisplay
  return typeof display === 'string' && display.trim() ? display : '-'
}

function shouldShowQuotaWindowLocalUsage(window?: any) {
  const totalTokens = window?.localUsage?.totalTokens
  return typeof totalTokens === 'number' && totalTokens > 0
}
</script>

<template>
  <section class="rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control)">
    <div class="mb-3 flex items-center justify-between gap-3">
      <div>
        <h3 class="m-0 text-[14px] font-[760] text-(--cp-text-primary)">账号额度</h3>
        <p
          class="m-0 mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
        >
          <span>Codex 额度</span>
          <span>·</span>
          <span>套餐:</span>
          <AccountPlanBadge :plan-type="account.planType" size="sm" />
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
      <div
        v-for="window in quotaWindows('monthly')"
        :key="window.key"
        class="rounded-lg bg-(--cp-bg-subtle) p-2"
      >
        <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
          <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
          <span class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums">
            <span
              v-if="shouldShowQuotaWindowLocalUsage(window)"
              class="text-[12px] font-[680] text-(--cp-text-muted)"
            >
              {{ quotaWindowLocalUsageDisplay(window) }}
            </span>
            <span class="text-[12px] font-[780]" :class="quotaWindowPercentTextClass(window)">
              {{ window.usedPercentDisplay }}
            </span>
          </span>
        </div>
        <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-default-border)">
          <div
            class="h-full rounded-full transition-[width,background-color] duration-200"
            :class="quotaWindowBarClass(window)"
            :style="quotaWindowBarStyle(window)"
          />
        </div>
        <div
          class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
        >
          <span>重置时间: {{ window.resetAtDisplay }}</span>
          <span>窗口已用: {{ window.windowUsedDisplay }}</span>
        </div>
      </div>

      <div v-if="quotaWindows('shortTerm').length > 0" class="grid gap-2">
        <div
          v-for="window in quotaWindows('shortTerm')"
          :key="window.key"
          class="rounded-lg bg-(--cp-bg-subtle) p-2"
        >
          <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
            <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
            <span class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums">
              <span
                v-if="shouldShowQuotaWindowLocalUsage(window)"
                class="text-[12px] font-[680] text-(--cp-text-muted)"
              >
                {{ quotaWindowLocalUsageDisplay(window) }}
              </span>
              <span class="text-[12px] font-[780]" :class="quotaWindowPercentTextClass(window)">
                {{ window.usedPercentDisplay }}
              </span>
            </span>
          </div>
          <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-default-border)">
            <div
              class="h-full rounded-full transition-[width,background-color] duration-200"
              :class="quotaWindowBarClass(window)"
              :style="quotaWindowBarStyle(window)"
            />
          </div>
          <div
            class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
          >
            <span>重置时间: {{ window.resetAtDisplay }}</span>
            <span>窗口已用: {{ window.windowUsedDisplay }}</span>
          </div>
        </div>
      </div>

      <div
        v-for="window in quotaWindows('other')"
        :key="window.key"
        class="rounded-lg bg-(--cp-bg-subtle) p-2"
      >
        <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
          <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
          <span class="flex shrink-0 items-baseline justify-end gap-1.5 font-mono tabular-nums">
            <span
              v-if="shouldShowQuotaWindowLocalUsage(window)"
              class="text-[12px] font-[680] text-(--cp-text-muted)"
            >
              {{ quotaWindowLocalUsageDisplay(window) }}
            </span>
            <span class="text-[12px] font-[780]" :class="quotaWindowPercentTextClass(window)">
              {{ window.usedPercentDisplay }}
            </span>
          </span>
        </div>
        <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-default-border)">
          <div
            class="h-full rounded-full transition-[width,background-color] duration-200"
            :class="quotaWindowBarClass(window)"
            :style="quotaWindowBarStyle(window)"
          />
        </div>
        <div
          class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
        >
          <span>重置时间: {{ window.resetAtDisplay }}</span>
          <span>窗口已用: {{ window.windowUsedDisplay }}</span>
        </div>
      </div>
    </div>
  </section>
</template>
