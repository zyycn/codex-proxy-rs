<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { LayoutGrid } from '@lucide/vue'

import { PROVIDER_DISPLAY_NAMES } from '@/utils/providers'
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
  { label: PROVIDER_DISPLAY_NAMES.openai, value: 'openai', icon: Openai },
  { label: PROVIDER_DISPLAY_NAMES.xai, value: 'xai', icon: Xai },
]
</script>

<template>
  <BaseSegmented
    v-model="provider"
    label="按平台筛选"
    :options="providerOptions"
    :disabled="disabled"
    display="icon"
  />
</template>
