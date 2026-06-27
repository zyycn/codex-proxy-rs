<script setup lang="ts">
import { levelColors, levelLabels } from '../constants'

defineProps<{
  level: string
  compact?: boolean
}>()

function levelLabel(level: string) {
  return levelLabels[level] || level
}

function levelClass(level: string) {
  return [
    levelColors[level]?.bg || 'bg-(--cp-bg-subtle)',
    levelColors[level]?.text || 'text-(--cp-text-secondary)',
  ]
}
</script>

<template>
  <span
    class="inline-flex items-center justify-center rounded-full px-2 leading-none font-bold"
    :class="[compact ? 'h-6 min-w-12 text-[12px]' : 'h-6 text-[12px]', levelClass(level)]"
  >
    {{ levelLabel(level) }}
  </span>
</template>
