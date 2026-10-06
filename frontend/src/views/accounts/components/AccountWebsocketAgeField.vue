<script setup lang="ts">
import { BaseFormItem, BaseInput } from '@codex-proxy/ui'

defineProps<{ disabled?: boolean }>()
const seconds = defineModel<string>({ required: true })
</script>

<template>
  <BaseFormItem label="WS 最大复用时间（秒）" description="留空使用系统设置，正在进行的响应不受影响">
    <BaseInput v-model="seconds" inputmode="decimal" placeholder="例如 240，支持最多三位小数" :disabled="disabled" />
  </BaseFormItem>
</template>
