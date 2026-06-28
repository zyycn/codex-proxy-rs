<script setup lang="ts">
import { computed } from 'vue'
import type { CSSProperties } from 'vue'

type FormGap = 'compact' | 'default' | 'loose'

const props = withDefaults(
  defineProps<{
    columns?: number | string
    gap?: FormGap
  }>(),
  {
    columns: 1,
    gap: 'default',
  },
)

const gapClasses: Record<FormGap, string> = {
  compact: 'gap-3',
  default: 'gap-4',
  loose: 'gap-5',
}

const formClasses = computed(() => ['base-form grid min-w-0 grid-cols-1', gapClasses[props.gap]])

const formStyle = computed<CSSProperties>(() => {
  if (typeof props.columns === 'number') {
    return {
      '--cp-form-columns':
        props.columns > 1 ? `repeat(${props.columns}, minmax(0, 1fr))` : 'minmax(0, 1fr)',
    } as CSSProperties
  }

  return {
    '--cp-form-columns': props.columns,
  } as CSSProperties
})
</script>

<template>
  <form :class="formClasses" :style="formStyle" @submit.prevent>
    <slot />
  </form>
</template>

<style scoped>
@media (min-width: 640px) {
  .base-form {
    grid-template-columns: var(--cp-form-columns);
  }
}
</style>
