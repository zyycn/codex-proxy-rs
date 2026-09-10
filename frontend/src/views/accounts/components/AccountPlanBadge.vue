<script setup lang="ts">
import { computed } from 'vue'

import { stableVisualIndex } from '../utils/visualTone'

const props = withDefaults(
  defineProps<{
    planType?: string | null
    planTypeDisplay: string
    size?: 'xs' | 'sm' | 'md'
  }>(),
  {
    planType: null,
    size: 'md',
  },
)

const planPalettes: Record<string, string> = {
  free: 'bg-cp-cyan-container text-cp-cyan-on-container',
  pro: 'bg-cp-purple-container-strong text-cp-purple-on-container',
  prolite: 'bg-cp-purple-container text-cp-purple-on-container',
}

const fallbackPalettes = [
  'bg-cp-blue-container text-cp-blue-on-container',
  'bg-cp-green-container text-cp-green-on-container',
  'bg-cp-cyan-container text-cp-cyan-on-container',
  'bg-cp-orange-container text-cp-orange-on-container',
] as const

const rawPlanType = computed(() => props.planType?.trim() || '')

const sizeClass = computed(() => {
  if (props.size === 'xs')
    return 'h-4.5 rounded-full px-1.5 text-[10px] font-bold'
  return props.size === 'sm'
    ? 'h-5 rounded-full px-1.75 text-cp-xs font-bold'
    : 'h-5.5 rounded-full px-2 text-cp-xs font-heavy'
})

const paletteClass = computed(() => {
  const key = rawPlanType.value.toLowerCase()
  const planPalette = planPalettes[key]
  if (planPalette)
    return planPalette

  return fallbackPalettes[stableVisualIndex(key, fallbackPalettes.length)]
})
</script>

<template>
  <span
    class="inline-flex shrink-0 items-center justify-center whitespace-nowrap leading-none shadow-cp-tertiary"
    :class="[sizeClass, paletteClass]"
    :title="rawPlanType || undefined"
  >
    <span>{{ planTypeDisplay }}</span>
  </span>
</template>
