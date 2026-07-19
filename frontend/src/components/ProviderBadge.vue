<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { computed } from 'vue'

const props = defineProps({
  provider: {
    type: String,
    default: '',
  },
})

const normalizedProvider = computed(() => props.provider.trim().toLowerCase())
const supportedProvider = computed(
  () => normalizedProvider.value === 'openai' || normalizedProvider.value === 'xai',
)
</script>

<template>
  <span
    v-if="supportedProvider"
    class="inline-flex h-5.5 shrink-0 items-center gap-1.25 text-[11px] leading-none font-[720] text-(--cp-text-primary) whitespace-nowrap"
  >
    <Openai v-if="normalizedProvider === 'openai'" aria-hidden="true" :width="13" :height="13" />
    <Xai v-else aria-hidden="true" :width="13" :height="13" />
    {{ normalizedProvider === 'openai' ? 'OpenAI' : 'xAI' }}
  </span>
  <span v-else class="text-[12px] font-[650] text-(--cp-text-secondary)">
    {{ provider || '—' }}
  </span>
</template>
