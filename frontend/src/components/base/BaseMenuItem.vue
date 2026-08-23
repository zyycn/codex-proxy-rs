<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    tone?: 'neutral' | 'destructive'
    disabled?: boolean
    loading?: boolean
  }>(),
  {
    tone: 'neutral',
    disabled: false,
    loading: false,
  },
)

defineSlots<{
  icon?: () => unknown
  loading?: () => unknown
  default: () => unknown
}>()

const classes = computed(() => [
  'flex min-h-9 w-full touch-manipulation items-center gap-2 rounded-cp-sm border-0 bg-transparent px-3 text-left text-cp leading-none font-emphasis outline-none transition-[background-color,color] duration-150 motion-reduce:transition-none',
  'focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-cp-control-outline',
  'disabled:cursor-not-allowed disabled:text-cp-text-disabled',
  props.tone === 'destructive'
    ? 'text-cp-error-text hover:bg-cp-error-bg-hover'
    : 'text-cp-text hover:bg-cp-bg-text-hover',
])
</script>

<template>
  <button
    type="button"
    :class="classes"
    :disabled="disabled || loading"
    :aria-busy="loading || undefined"
  >
    <span v-if="loading" class="inline-grid size-3.5 shrink-0 place-items-center" aria-hidden="true">
      <slot name="loading">
        <LoaderCircle class="size-3.5 animate-spin motion-reduce:animate-none" />
      </slot>
    </span>
    <span v-else-if="$slots.icon" class="inline-grid shrink-0 place-items-center" aria-hidden="true">
      <slot name="icon" />
    </span>
    <span class="min-w-0 flex-1 truncate"><slot /></span>
  </button>
</template>
