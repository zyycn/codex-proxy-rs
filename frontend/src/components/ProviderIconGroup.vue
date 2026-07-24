<script setup lang="ts">
import { Key, Openai, Robot, Xai } from '@boxicons/vue'
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    provider?: string | null
    authenticationKind?: string | null
    size?: 'sm' | 'md'
  }>(),
  {
    size: 'md',
  },
)

const normalizedProvider = computed(() => (props.provider ?? '').trim().toLowerCase())
const normalizedAuthenticationKind = computed(() =>
  (props.authenticationKind ?? '').trim().toLowerCase(),
)
const showAuthenticationKind = computed(() => props.authenticationKind !== undefined)
const groupGapClass = computed(() => (props.size === 'sm' ? 'gap-1' : 'gap-2'))
const iconContainerClass = computed(() =>
  props.size === 'sm' ? 'size-5 rounded-md' : 'size-7 rounded-lg',
)
const iconClass = computed(() => (props.size === 'sm' ? 'size-3' : 'size-4'))

const providerLabel = computed(() => {
  if (normalizedProvider.value === 'openai')
    return 'OpenAI'
  if (normalizedProvider.value === 'xai')
    return 'xAI'
  return props.provider?.trim() || '未知平台'
})

const authenticationLabel = computed(() => {
  if (normalizedAuthenticationKind.value === 'agent_identity')
    return 'Agent Identity'
  if (normalizedAuthenticationKind.value === 'oauth')
    return 'OAuth'
  return props.authenticationKind?.trim() || '未知认证类型'
})
</script>

<template>
  <span class="inline-flex items-center whitespace-nowrap" :class="groupGapClass">
    <span
      class="inline-flex shrink-0 items-center justify-center bg-(--cp-bg-muted) text-(--cp-text-primary)"
      :class="iconContainerClass"
      :title="providerLabel"
    >
      <Openai v-if="normalizedProvider === 'openai'" :class="iconClass" aria-hidden="true" />
      <Xai v-else-if="normalizedProvider === 'xai'" :class="iconClass" aria-hidden="true" />
      <span v-else class="text-[10px] font-[760] text-(--cp-text-muted)">?</span>
    </span>

    <span
      v-if="showAuthenticationKind"
      class="inline-flex shrink-0 items-center justify-center bg-(--cp-bg-muted) text-(--cp-text-primary)"
      :class="iconContainerClass"
      :title="authenticationLabel"
    >
      <Key
        v-if="normalizedAuthenticationKind === 'oauth'"
        :class="iconClass"
        aria-hidden="true"
      />
      <Robot
        v-else-if="normalizedAuthenticationKind === 'agent_identity'"
        :class="iconClass"
        aria-hidden="true"
      />
      <span v-else class="text-[10px] font-[760] text-(--cp-text-muted)">?</span>
    </span>

    <span class="sr-only">
      {{ providerLabel }}<template v-if="showAuthenticationKind">，{{ authenticationLabel }}</template>
    </span>
  </span>
</template>
