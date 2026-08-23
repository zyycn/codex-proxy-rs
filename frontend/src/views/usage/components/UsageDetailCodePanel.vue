<script setup lang="ts">
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'

// 详情弹窗的代码面板：标题 + 滚动区内的预格式化文本。
defineProps<{
  title: string
  maxHeight: string
  content: string
}>()

const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'
const codeBlockClass
  = 'm-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.65] text-cp-text'
</script>

<template>
  <h3 class="mb-3" :class="panelTitleClass">
    {{ title }}
  </h3>
  <BaseScrollbar :max-height="maxHeight">
    <div class="rounded-cp bg-cp-bg-container px-3 py-2.5">
      <pre :class="codeBlockClass">{{ content }}</pre>
    </div>
  </BaseScrollbar>
</template>
