<script setup lang="ts">
import { Info } from '@lucide/vue'

import BasePopover from '@/components/base/BasePopover.vue'

// 表格单元格的“信息点”悬浮明细：统一触发按钮与面板壳。
defineProps<{
  title: string
  triggerLabel: string
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
        class="inline-flex size-4 items-center justify-center rounded-full bg-cp-info-bg text-cp-info outline-none hover:bg-cp-default-hover"
        :aria-label="triggerLabel"
      >
        <Info class="size-3" />
      </button>
    </template>

    <div class="grid w-60 gap-2 p-3 text-[12px] leading-none">
      <p class="m-0 font-heavy text-cp-primary">
        {{ title }}
      </p>
      <slot />
    </div>
  </BasePopover>
</template>
