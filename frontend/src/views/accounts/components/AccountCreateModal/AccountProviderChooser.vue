<script setup lang="ts">
import type { AccountCreateProvider } from './model'
import { Openai, Xai } from '@boxicons/vue'
import { LayoutGrid } from '@lucide/vue'
import { PROVIDER_DISPLAY_NAMES } from '@/utils/providers'

withDefaults(
  defineProps<{
    disabled?: boolean
    selected?: AccountCreateProvider | ''
  }>(),
  {
    disabled: false,
  },
)

const emit = defineEmits<{
  select: [provider: 'openai' | 'xai' | 'batch']
}>()

const providers = [
  {
    value: 'batch' as const,
    label: '批量导入',
    icon: LayoutGrid,
  },
  {
    value: 'openai' as const,
    label: PROVIDER_DISPLAY_NAMES.openai,
    icon: Openai,
  },
  {
    value: 'xai' as const,
    label: PROVIDER_DISPLAY_NAMES.xai,
    icon: Xai,
  },
]
</script>

<template>
  <div class="grid grid-cols-3 gap-3" role="group" aria-label="选择账号平台">
    <button
      v-for="provider in providers"
      :key="provider.value"
      type="button"
      class="flex min-w-0 cursor-pointer flex-col items-center gap-3 rounded-cp border-0 px-2 py-4 text-cp font-medium outline-none transition-colors duration-150 focus-visible:ring-2 focus-visible:ring-cp-control-outline disabled:cursor-not-allowed disabled:opacity-55 motion-reduce:transition-none"
      :class="selected === provider.value ? 'bg-cp-primary-container text-cp-primary-on-container' : 'bg-cp-fill-quaternary text-cp-text-secondary hover:bg-cp-fill-tertiary hover:text-cp-text'"
      :disabled="disabled"
      :aria-pressed="selected === provider.value"
      @click="emit('select', provider.value)"
    >
      <component :is="provider.icon" :width="24" :height="24" aria-hidden="true" />
      <span>{{ provider.label }}</span>
    </button>
  </div>
</template>
