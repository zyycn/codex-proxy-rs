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

const palettes = [
  'bg-(--cp-info-bg) text-(--cp-info-text) shadow-(--cp-shadow-input)',
  'bg-(--cp-success-bg) text-(--cp-success-text) shadow-(--cp-shadow-input)',
  'bg-(--cp-normal-bg) text-(--cp-normal-text) shadow-(--cp-shadow-input)',
  'bg-(--cp-warning-bg) text-(--cp-warning-text) shadow-(--cp-shadow-input)',
]

const label = computed(() => props.planType?.trim() || 'Free')

const sizeClass = computed(() =>
  props.size === 'sm'
    ? 'h-5 max-w-24 rounded-full px-1.75 text-[11px] font-[720]'
    : 'h-5.5 max-w-full rounded-full px-2 text-[11px] font-[760]',
)

const paletteClass = computed(() => {
  const key = label.value.toLowerCase()
  let hash = 0
  for (const char of key) {
    hash += char.charCodeAt(0)
  }
  return palettes[hash % palettes.length]
})
</script>

<template>
  <span
    class="inline-flex min-w-0 items-center justify-center leading-none capitalize"
    :class="[sizeClass, paletteClass]"
  >
    <span class="min-w-0 truncate">{{ label }}</span>
  </span>
</template>
