<script setup lang="ts">
import { Info } from '@lucide/vue'

import BasePopover from '@/components/base/BasePopover.vue'

// 表格单元格的“信息点”悬浮明细：统一触发按钮与面板壳，
// 各单元格只提供标题、宽度与明细行内容。
defineProps<{
  title: string
  width: string
  triggerLabel: string
}>()
</script>

<template>
  <BasePopover
    trigger="hover"
    placement="right"
    :width="width"
    panel-class="!p-3 text-(--cp-text-primary)"
  >
    <template #trigger>
      <button
        type="button"
        class="inline-flex size-4 items-center justify-center rounded-full bg-(--cp-info-bg) text-(--cp-info) outline-none hover:bg-(--cp-default-bg-hover)"
        :aria-label="triggerLabel"
      >
        <Info class="size-3" />
      </button>
    </template>

    <div class="grid gap-2 text-[12px] leading-none">
      <p class="m-0 font-heavy text-(--cp-text-primary)">
        {{ title }}
      </p>
      <slot />
    </div>
  </BasePopover>
</template>
