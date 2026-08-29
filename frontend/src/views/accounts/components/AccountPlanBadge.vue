<script setup lang="ts">
import { computed } from 'vue'

import { stableVisualIndex } from '../utils/visualTone'

const props = withDefaults(
  defineProps<{
    planType?: string | null
    size?: 'sm' | 'md'
  }>(),
  {
    planType: null,
    size: 'md',
  },
)

const planPalettes: Record<string, string> = {
  free: 'bg-cp-cyan-bg text-cp-cyan-text-on-bg',
  pro: 'bg-cp-purple-bg-strong text-cp-purple-text-on-bg',
  prolite: 'bg-cp-purple-bg text-cp-purple-text-on-bg',
}

const fallbackPalettes = [
  'bg-cp-blue-bg text-cp-blue-text-on-bg',
  'bg-cp-green-bg text-cp-green-text-on-bg',
  'bg-cp-cyan-bg text-cp-cyan-text-on-bg',
  'bg-cp-orange-bg text-cp-orange-text-on-bg',
] as const

const label = computed(() => props.planType?.trim() || 'Free')

const sizeClass = computed(() =>
  props.size === 'sm'
    ? 'h-5 rounded-full px-1.75 text-cp-xs font-bold'
    : 'h-5.5 rounded-full px-2 text-cp-xs font-heavy',
)

const paletteClass = computed(() => {
  const key = label.value.toLowerCase()
  const planPalette = planPalettes[key]
  if (planPalette)
    return planPalette

  return fallbackPalettes[stableVisualIndex(key, fallbackPalettes.length)]
})
</script>

<template>
  <span
    class="inline-flex shrink-0 items-center justify-center whitespace-nowrap leading-none capitalize shadow-cp-tertiary"
    :class="[sizeClass, paletteClass]"
  >
    <span>{{ label }}</span>
  </span>
</template>
