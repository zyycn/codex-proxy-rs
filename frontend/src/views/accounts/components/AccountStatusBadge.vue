<script setup lang="ts">
import { computed } from 'vue'

import { statusLabels, statusTones } from '../constants'

const props = withDefaults(
  defineProps<{
    status: string
    variant?: 'inline' | 'pill'
  }>(),
  {
    variant: 'inline',
  },
)

const tone = computed(() => statusTones[props.status])
const label = computed(() => statusLabels[props.status] || props.status)

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
  <span
    v-if="variant === 'pill'"
    class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-[760]"
    :class="textClass"
  >
    {{ label }}
  </span>
  <span
    v-else
    class="inline-flex min-w-16 items-center gap-1.5 text-[12px] leading-none font-[650]"
    :class="textClass"
  >
    <span class="size-1.5 rounded-full" :class="dotClass" />
    <span>{{ label }}</span>
  </span>
</template>
