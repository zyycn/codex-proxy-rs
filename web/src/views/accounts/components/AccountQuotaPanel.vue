<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'

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
  return Math.max(0, Math.min(window?.usedPercent ?? 0, 100))
}

function quotaWindowBarWidth(window?: any) {
  return `${quotaWindowPercent(window)}%`
}
</script>

<template>
  <section class="rounded-lg bg-(--cp-bg-surface) p-4 shadow-(--cp-shadow-control)">
    <div class="mb-3 flex items-center justify-between gap-3">
      <div>
        <h3 class="m-0 text-[14px] font-[760] text-(--cp-text-primary)">账号额度</h3>
        <p class="m-0 mt-1 text-[12px] font-[620] text-(--cp-text-secondary)">
          Codex 额度 · 套餐: {{ account.planType || 'Free' }} · 最近刷新:
          {{ account.quota.refreshedAtDisplay }}
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
          <span class="text-(--cp-text-primary)">{{ window.usedPercentDisplay }}</span>
        </div>
        <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
          <div
            class="h-full rounded-full bg-(--cp-info)"
            :style="{ width: quotaWindowBarWidth(window) }"
          />
        </div>
        <div
          class="mt-3 flex flex-wrap justify-between gap-x-3 gap-y-1 text-[12px] font-[620] text-(--cp-text-secondary)"
        >
          <span>重置时间: {{ window.resetAtDisplay }}</span>
          <span>窗口已用: {{ window.windowUsedDisplay }}</span>
        </div>
      </div>

      <div v-if="quotaWindows('shortTerm').length > 0" class="grid gap-2 sm:grid-cols-2">
        <div
          v-for="window in quotaWindows('shortTerm')"
          :key="window.key"
          class="rounded-lg bg-(--cp-bg-subtle) p-2"
        >
          <div class="flex items-center justify-between gap-3 text-[12px] font-[720]">
            <span class="text-(--cp-text-secondary)">{{ window.labelDisplay }}</span>
            <span class="text-(--cp-text-primary)">{{ window.usedPercentDisplay }}</span>
          </div>
          <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
            <div
              class="h-full rounded-full bg-(--cp-info)"
              :style="{ width: quotaWindowBarWidth(window) }"
            />
          </div>
          <div class="mt-3 flex flex-col gap-1 text-[12px] font-[620] text-(--cp-text-secondary)">
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
          <span class="text-(--cp-text-primary)">{{ window.usedPercentDisplay }}</span>
        </div>
        <div class="mt-2 h-2 overflow-hidden rounded-full bg-(--cp-bg-tertiary)">
          <div
            class="h-full rounded-full bg-(--cp-info)"
            :style="{ width: quotaWindowBarWidth(window) }"
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
