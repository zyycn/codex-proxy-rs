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

const palettes = {
  blue: 'bg-z-blue-container text-z-blue-on-container',
  green: 'bg-z-green-container text-z-green-on-container',
  cyan: 'bg-z-cyan-container text-z-cyan-on-container',
  orange: 'bg-z-orange-container text-z-orange-on-container',
  cyanStrong: 'bg-z-cyan-container-strong text-z-cyan-on-container',
  greenStrong: 'bg-z-green-container-strong text-z-green-on-container',
}

const fallbackPalettes = [
  palettes.blue,
  palettes.green,
  palettes.cyan,
  palettes.orange,
]

// 已知套餐按展示名配色，Provider 的不同别名共享颜色
const planPalettes = new Map([
  ['api', palettes.cyanStrong],
  ['free', palettes.cyan],
  ['go', palettes.cyan],
  ['plus', 'bg-z-blue-container-strong text-z-blue-on-container'],
  ['prolite', 'bg-z-purple-container text-z-purple-on-container'],
  ['pro', 'plan-pro text-z-purple-on-container'],
  ['promax', 'bg-z-purple-container-strong text-z-purple-on-container'],
  ['business', palettes.orange],
  ['business premium', 'bg-z-orange-container-strong text-z-orange-on-container'],
  ['enterprise', palettes.blue],
  ['enterprise (automation)', palettes.blue],
  ['edu', palettes.green],
  ['edu plus', palettes.greenStrong],
  ['edu pro', palettes.greenStrong],
  ['academic researchers', palettes.cyan],
  ['academic researchers plus', palettes.cyanStrong],
])

const displayText = computed(() => props.authenticationKind === 'api_key'
  ? 'API'
  : props.planTypeDisplay)

const sizes = {
  xs: { text: 'text-[10px] font-bold', singleLine: 'h-4.5 px-1.5' },
  sm: { text: 'text-z-xs font-bold', singleLine: 'h-5 px-1.75' },
  md: { text: 'text-z-xs font-heavy', singleLine: 'h-5.5 px-2' },
}

const paletteClass = computed(() => {
  const key = props.planType?.trim().toLowerCase() || ''
  return planPalettes.get(displayText.value?.trim().toLowerCase() || key)
    ?? fallbackPalettes[stableVisualIndex(key, fallbackPalettes.length)]
})
</script>

<template>
  <AccountUpstreamBadge v-if="upstreamPeer || upstream?.discovered" :size="size" />
  <span
    v-else-if="wrap"
    class="line-clamp-3 max-w-full py-0.75 text-center leading-4 text-balance wrap-anywhere whitespace-normal"
    :class="sizes[size].text"
    :title="displayText || undefined"
  >
    <span class="box-decoration-clone rounded-md px-1.25 py-0.75" :class="paletteClass">
      {{ displayText }}
      <span v-if="count !== undefined" class="tabular-nums">{{ count }}</span>
    </span>
  </span>
  <span
    v-else
    class="inline-flex min-w-0 max-w-full items-center justify-center gap-1 rounded-full whitespace-nowrap leading-none shadow-z-tertiary"
    :class="[sizes[size].text, sizes[size].singleLine, paletteClass]"
    :title="displayText || undefined"
  >
    <span class="truncate">{{ displayText }}</span>
    <span v-if="count !== undefined" class="shrink-0 tabular-nums">{{ count }}</span>
  </span>
</template>

<style scoped>
.plan-pro {
  background-color: color-mix(in srgb, var(--z-color-purple-container), var(--z-color-purple-container-strong));
}
</style>
