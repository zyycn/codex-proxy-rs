<script setup lang="ts">
import { displayValue, fieldLabelClass, fieldValueClass } from '../utils/detail'

// 详情弹窗的两列字段网格：label + 值（可选等宽字体），空值以“—”占位。
defineProps<{
  items: Array<{ label: string, value: unknown, mono?: boolean }>
}>()
</script>

<template>
  <dl class="mt-3 grid grid-cols-1 gap-x-4 gap-y-3 sm:grid-cols-2">
    <div v-for="item in items" :key="item.label" class="min-w-0">
      <dt :class="fieldLabelClass">
        {{ item.label }}
      </dt>
      <dd :class="fieldValueClass(item.mono)" :title="displayValue(item.value)">
        {{ displayValue(item.value) }}
      </dd>
    </div>
  </dl>
</template>
