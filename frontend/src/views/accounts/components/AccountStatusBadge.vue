<script setup lang="ts">
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { errorReasonLabels, statusLabels, statusTones } from '../constants'

const props = withDefaults(
  defineProps<{
    status: string
    /** `status === 'error'` 时的具体原因（对应后端 `errorReason`）。 */
    errorReason?: string | null
    /** 最近一次失败的上游错误原文（对应后端 `errorMessage`），作为辅助证据展示。 */
    errorMessage?: string | null
    variant?: 'inline' | 'pill'
  }>(),
  {
    errorReason: null,
    errorMessage: null,
    variant: 'inline',
  },
)

const tone = computed(() => statusTones[props.status])
const label = computed(() => statusLabels[props.status] || props.status)
/** 分类原因（受控枚举映射文案）；`status === 'error'` 时有值。 */
const reasonLabel = computed(() => {
  if (props.errorReason)
    return errorReasonLabels[props.errorReason] || props.errorReason
  return null
})
/** 上游错误原文（辅助证据）；有则显、无则隐。 */
const errorText = computed(() => props.errorMessage || null)
const hasDetail = computed(() => Boolean(reasonLabel.value) || Boolean(errorText.value))

const textClass = computed(() => {
  if (tone.value === 'success') {
    return 'text-(--cp-success-text)'
  }
  if (tone.value === 'danger') {
    return 'text-(--cp-danger-text)'
  }
  if (tone.value === 'warning') {
    return 'text-(--cp-warning-text)'
  }
  if (tone.value === 'info') {
    return 'text-(--cp-info-text)'
  }
  return 'text-(--cp-text-secondary)'
})

const dotClass = computed(() => {
  if (tone.value === 'success') {
    return 'bg-(--cp-success)'
  }
  if (tone.value === 'danger') {
    return 'bg-(--cp-danger)'
  }
  if (tone.value === 'warning') {
    return 'bg-(--cp-warning)'
  }
  if (tone.value === 'info') {
    return 'bg-(--cp-info)'
  }
  return 'bg-(--cp-text-muted)'
})
</script>

<template>
  <BasePopover
    :disabled="!hasDetail"
    trigger="hover"
    placement="top-start"
    :width="240"
    panel-class="p-2"
  >
    <template #trigger>
      <span
        v-if="variant === 'pill'"
        class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-heavy"
        :class="textClass"
      >
        {{ label }}
      </span>
      <span
        v-else
        class="inline-flex min-w-16 items-center gap-1.5 text-[12px] leading-none font-emphasis"
        :class="textClass"
      >
        <span class="size-1.5 rounded-full" :class="dotClass" />
        <span>{{ label }}</span>
      </span>
    </template>
    <p v-if="reasonLabel" class="m-0 text-[12px] font-medium leading-relaxed text-(--cp-text-primary)">
      {{ reasonLabel }}
    </p>
    <p v-if="errorText" class="m-0 mt-1 text-[12px] leading-relaxed text-(--cp-text-secondary)">
      {{ errorText }}
    </p>
  </BasePopover>
</template>
