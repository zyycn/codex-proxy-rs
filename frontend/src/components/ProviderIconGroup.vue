<script setup lang="ts">
import { computed } from 'vue'
import { authenticationIcon, formatAuthenticationLabel, formatProviderLabel, providerIcon } from '@/utils/providers'

const props = withDefaults(
  defineProps<{
    provider?: string | null
    authenticationKind?: string | null
    size?: 'xs' | 'sm' | 'md'
  }>(),
  {
    size: 'md',
  },
)

const normalizedProvider = computed(() => (props.provider ?? '').trim().toLowerCase())
const providerIconComponent = computed(() => normalizedProvider.value ? providerIcon(normalizedProvider.value) : undefined)
const authenticationIconComponent = computed(() => authenticationIcon(props.authenticationKind))
const showAuthenticationKind = computed(() => props.authenticationKind !== undefined)
const groupGapClass = computed(() => {
  if (props.size === 'xs')
    return 'gap-0.75'
  return props.size === 'sm' ? 'gap-1' : 'gap-2'
})
const iconContainerClass = computed(() => {
  if (props.size === 'xs')
    return 'size-4.5 rounded-md'
  return props.size === 'sm' ? 'size-5 rounded-md' : 'size-7 rounded-lg'
})
const iconClass = computed(() => {
  if (props.size === 'xs')
    return 'size-2.5'
  return props.size === 'sm' ? 'size-3' : 'size-4'
})

const providerLabel = computed(() => formatProviderLabel(props.provider, '未知平台'))

const authenticationLabel = computed(() => formatAuthenticationLabel(props.authenticationKind))
</script>

<template>
  <span class="inline-flex items-center whitespace-nowrap" :class="groupGapClass">
    <span
      class="inline-flex shrink-0 items-center justify-center bg-cp-fill-secondary/80 text-cp-text"
      :class="iconContainerClass"
      :title="providerLabel"
    >
      <component :is="providerIconComponent" v-if="providerIconComponent" :class="iconClass" />
      <span v-else class="text-[10px] font-heavy text-cp-text-quaternary">?</span>
    </span>

    <span
      v-if="showAuthenticationKind"
      class="inline-flex shrink-0 items-center justify-center bg-cp-fill-secondary/80 text-cp-text"
      :class="iconContainerClass"
      :title="authenticationLabel"
    >
      <component :is="authenticationIconComponent" v-if="authenticationIconComponent" :class="iconClass" />
      <span v-else class="text-[10px] font-heavy text-cp-text-quaternary">?</span>
    </span>

    <span class="sr-only">
      {{ providerLabel }}<template v-if="showAuthenticationKind">，{{ authenticationLabel }}</template>
    </span>
  </span>
</template>
