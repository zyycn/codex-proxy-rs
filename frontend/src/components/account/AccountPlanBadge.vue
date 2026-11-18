<script setup lang="ts">
import type { AccountUpstream } from '@/api'
import { computed } from 'vue'

import { stableVisualIndex } from '@/utils/color'
import AccountUpstreamBadge from './AccountUpstreamBadge.vue'

const props = withDefaults(
  defineProps<{
    planType?: string | null
    authenticationKind?: string
    upstream?: AccountUpstream | null
    upstreamPeer?: boolean | null
    planTypeDisplay: string
    count?: number
    size?: 'xs' | 'sm' | 'md'
    wrap?: boolean
  }>(),
  {
    planType: null,
    size: 'md',
  },
)

const planPalettes: Record<string, string> = {
  api: 'bg-z-cyan-container-strong text-z-cyan-on-container',
  free: 'bg-z-cyan-container text-z-cyan-on-container',
  plus: 'bg-z-blue-container-strong text-z-blue-on-container',
  pro: 'bg-z-purple-container-strong text-z-purple-on-container',
  prolite: 'bg-z-purple-container text-z-purple-on-container',
}

const fallbackPalettes = [
  'bg-z-blue-container text-z-blue-on-container',
  'bg-z-green-container text-z-green-on-container',
  'bg-z-cyan-container text-z-cyan-on-container',
  'bg-z-orange-container text-z-orange-on-container',
] as const

const rawPlanType = computed(() => props.authenticationKind === 'api_key'
  ? 'api'
  : props.planType?.trim() || '')
const displayText = computed(() => props.authenticationKind === 'api_key'
  ? 'API'
  : props.planTypeDisplay)

const sizes = {
  xs: { text: 'text-[10px] font-bold', singleLine: 'h-4.5 px-1.5' },
  sm: { text: 'text-z-xs font-bold', singleLine: 'h-5 px-1.75' },
  md: { text: 'text-z-xs font-heavy', singleLine: 'h-5.5 px-2' },
}

const paletteClass = computed(() => {
  const key = rawPlanType.value.toLowerCase()
  const planPalette = planPalettes[key]
  if (planPalette)
    return planPalette

  return fallbackPalettes[stableVisualIndex(key, fallbackPalettes.length)]
})
</script>

<template>
  <AccountUpstreamBadge v-if="upstreamPeer || upstream?.discovered" :size="size" />
  <span
    v-else
    class="inline-flex min-w-0 max-w-full items-center justify-center gap-1 shadow-z-tertiary"
    :class="[
      sizes[size].text, paletteClass,
      wrap
        ? 'min-h-5.5 rounded-lg px-1 py-0.75 text-center leading-3.5'
        : [sizes[size].singleLine, 'rounded-full whitespace-nowrap leading-none'],
    ]"
    :title="displayText || undefined"
  >
    <span :class="wrap ? 'line-clamp-3 text-balance wrap-anywhere whitespace-normal' : 'truncate'">{{ displayText }}</span>
    <span v-if="count !== undefined" class="shrink-0 tabular-nums">{{ count }}</span>
  </span>
</template>
