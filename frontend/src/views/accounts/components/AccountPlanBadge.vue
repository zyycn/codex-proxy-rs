<script setup lang="ts">
import { computed } from 'vue'

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

const fallbackPalettes = [
  'bg-cp-info-bg text-cp-info-text shadow-cp-input',
  'bg-cp-success-bg text-cp-success-text shadow-cp-input',
  'bg-cp-status-normal-bg text-cp-status-normal-text shadow-cp-input',
  'bg-cp-warning-bg text-cp-warning-text shadow-cp-input',
]

const usageTierPalettes: Record<string, string> = {
  pro: 'bg-cp-account-plan-pro-bg text-cp-account-plan-pro shadow-cp-input',
  prolite: 'bg-cp-account-plan-pro-lite-bg text-cp-account-plan-pro-lite shadow-cp-input',
}

const label = computed(() => props.planType?.trim() || 'Free')

const sizeClass = computed(() =>
  props.size === 'sm'
    ? 'h-5 rounded-full px-1.75 text-cp-xs font-bold'
    : 'h-5.5 rounded-full px-2 text-cp-xs font-heavy',
)

const paletteClass = computed(() => {
  const key = label.value.toLowerCase()
  const usageTierPalette = usageTierPalettes[key]
  if (usageTierPalette)
    return usageTierPalette

  let hash = 0
  for (const char of key) {
    hash += char.charCodeAt(0)
  }
  return fallbackPalettes[hash % fallbackPalettes.length]
})
</script>

<template>
  <span
    class="inline-flex shrink-0 items-center justify-center whitespace-nowrap leading-none capitalize"
    :class="[sizeClass, paletteClass]"
  >
    <span>{{ label }}</span>
  </span>
</template>
