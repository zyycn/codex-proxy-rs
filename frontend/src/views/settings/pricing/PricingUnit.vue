<script setup lang="ts">
import { Info } from '@lucide/vue'
import { shallowRef, useId } from 'vue'
import BasePopover from '@/components/base/BasePopover.vue'

const infoOpen = shallowRef(false)
const infoId = useId()
</script>

<template>
  <BasePopover v-model="infoOpen" placement="bottom-start" trigger="hover-click">
    <template #trigger>
      <button
        type="button"
        class="inline-flex h-6 cursor-pointer touch-manipulation items-center gap-1.5 rounded-cp-sm border-0 bg-transparent p-0 text-cp-xs font-normal text-cp-text-tertiary outline-none transition-colors hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
        aria-label="价格单位：美元 / 百万 Token，查看定价说明"
        :aria-expanded="infoOpen"
        :aria-controls="infoOpen ? infoId : undefined"
      >
        <span aria-hidden="true">USD / 1M Tokens</span>
        <Info class="size-3.5" aria-hidden="true" />
      </button>
    </template>
    <div :id="infoId" class="grid w-72 gap-2 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
      <p class="m-0 font-emphasis text-cp-text">
        单价单位：USD / 1M Tokens
      </p>
      <p class="m-0">
        人工覆盖优先于同步价目，再继承内置价格。仅影响本地估算与金额限额。
      </p>
      <p class="m-0">
        表中为标准档生效价；图像模型显示图像 Token 价格。划线为来源价，其他档位在编辑中查看。
      </p>
    </div>
  </BasePopover>
</template>
