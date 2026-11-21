<script setup lang="ts">
import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
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

const { t } = useI18n({ useScope: 'global' })

const normalizedProvider = computed(() => (props.provider ?? '').trim().toLowerCase())
const providerIconComponent = computed(() => normalizedProvider.value
  ? providerIcon(normalizedProvider.value)
  : undefined)
const authenticationIconComponent = computed(() => authenticationIcon(props.authenticationKind))
const showAuthenticationKind = computed(() => props.authenticationKind !== undefined)
const sizeClasses = {
  xs: { gap: 'gap-0.75', container: 'size-4.5 rounded-md', icon: 'size-2.5' },
  sm: { gap: 'gap-1', container: 'size-5 rounded-md', icon: 'size-3' },
  md: { gap: 'gap-2', container: 'size-7 rounded-lg', icon: 'size-4' },
}
const classes = computed(() => sizeClasses[props.size])

const providerLabel = computed(() => formatProviderLabel(props.provider, t('common.providers.unknown')))

const authenticationLabel = computed(() => formatAuthenticationLabel(props.authenticationKind))
</script>

<template>
  <span class="inline-flex items-center whitespace-nowrap" :class="classes.gap">
    <span
      class="inline-flex shrink-0 items-center justify-center bg-z-fill-secondary/80 text-z-text"
      :class="classes.container"
      :title="providerLabel"
    >
      <component :is="providerIconComponent" v-if="providerIconComponent" :class="classes.icon" />
      <span v-else class="text-[10px] font-heavy text-z-text-quaternary">?</span>
    </span>

    <span
      v-if="showAuthenticationKind"
      class="inline-flex shrink-0 items-center justify-center bg-z-fill-secondary/80 text-z-text"
      :class="classes.container"
      :title="authenticationLabel"
    >
      <component :is="authenticationIconComponent" v-if="authenticationIconComponent" :class="classes.icon" />
      <span v-else class="text-[10px] font-heavy text-z-text-quaternary">?</span>
    </span>

    <span class="sr-only">
      {{ providerLabel }}<template v-if="showAuthenticationKind">，{{ authenticationLabel }}</template>
    </span>
  </span>
</template>
