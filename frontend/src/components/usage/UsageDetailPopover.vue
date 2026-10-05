<script setup lang="ts">
import { BasePopover } from '@codex-proxy/ui'

import { Info } from '@lucide/vue'

// 表格单元格的“信息点”悬浮明细：统一触发按钮与面板壳。
defineProps<{
  title: string
  triggerLabel: string
  tone?: 'primary' | 'warning'
}>()
</script>

<template>
  <BasePopover
    trigger="hover"
    placement="right"
  >
    <template #trigger>
      <button
        type="button"
        class="inline-flex size-4 items-center justify-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :class="tone === 'warning'
          ? 'bg-cp-warning-container text-cp-warning-on-container hover:bg-cp-warning-container-hover'
          : 'bg-cp-primary-container text-cp-primary-on-container hover:bg-cp-primary-container-hover'"
        :aria-label="triggerLabel"
      >
        <Info class="size-3" />
      </button>
    </template>

    <div class="grid w-60 gap-2 p-3 text-cp-sm leading-none">
      <p class="m-0 font-heavy text-cp-text">
        {{ title }}
      </p>
      <slot />
    </div>
  </BasePopover>
</template>
