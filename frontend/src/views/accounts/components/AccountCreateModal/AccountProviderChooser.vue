<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { LayoutGrid } from '@lucide/vue'
import { PROVIDER_DISPLAY_NAMES } from '@/utils/providers'

withDefaults(
  defineProps<{
    disabled?: boolean
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
  <div class="flex items-center justify-center gap-4 sm:gap-8" role="group" aria-label="选择账号平台">
    <button
      v-for="provider in providers"
      :key="provider.value"
      type="button"
      class="group inline-flex size-[88px] cursor-pointer items-center justify-center rounded-cp-lg border-0 bg-transparent text-cp-text outline-none transition-colors duration-150 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-cp-border disabled:cursor-not-allowed disabled:opacity-55"
      :disabled="disabled"
      :aria-label="`导入 ${provider.label} 账号`"
      :title="provider.label"
      @click="emit('select', provider.value)"
    >
      <span
        class="inline-flex size-16 items-center justify-center rounded-cp-lg bg-cp-fill-quaternary transition-colors duration-150 group-hover:bg-cp-fill-tertiary"
      >
        <component :is="provider.icon" :width="36" :height="36" />
      </span>
    </button>
  </div>
</template>
