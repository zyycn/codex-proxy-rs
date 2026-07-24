<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { LayoutGrid } from '@lucide/vue'

import BaseSegmented from './base/BaseSegmented.vue'

withDefaults(
  defineProps<{
    disabled?: boolean
  }>(),
  {
    disabled: false,
  },
)

const provider = defineModel<string>({ required: true })

const providerOptions = [
  { label: '全部平台', value: '', icon: LayoutGrid },
  { label: 'OpenAI', value: 'openai', icon: Openai },
  { label: 'xAI', value: 'xai', icon: Xai },
]
</script>

<template>
  <BaseSegmented
    v-model="provider"
    :options="providerOptions"
    :disabled="disabled"
    icon-only
    size="input"
    aria-label="按平台筛选"
  />
</template>
