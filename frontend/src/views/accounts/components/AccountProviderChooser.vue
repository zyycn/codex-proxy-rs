<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'

withDefaults(
  defineProps<{
    disabled?: boolean
  }>(),
  {
    disabled: false,
  },
)

const emit = defineEmits<{
  select: [provider: 'openai' | 'xai']
}>()

const providers = [
  {
    value: 'openai' as const,
    label: 'OpenAI',
    icon: Openai,
  },
  {
    value: 'xai' as const,
    label: 'xAI',
    icon: Xai,
  },
]
</script>

<template>
  <div
    class="flex items-center justify-center gap-4 sm:gap-8"
    role="group"
    aria-label="选择账号平台"
  >
    <button
      v-for="provider in providers"
      :key="provider.value"
      type="button"
      class="group inline-flex size-[88px] cursor-pointer items-center justify-center rounded-(--cp-panel-radius) border-0 bg-transparent text-(--cp-text-primary) outline-none transition-colors duration-150 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-(--cp-default-border-hover) disabled:cursor-not-allowed disabled:opacity-55"
      :disabled="disabled"
      :aria-label="`导入 ${provider.label} 账号`"
      :title="provider.label"
      @click="emit('select', provider.value)"
    >
      <span
        class="inline-flex size-16 items-center justify-center rounded-(--cp-panel-radius) bg-(--cp-bg-subtle) transition-colors duration-150 group-hover:bg-(--cp-bg-muted)"
      >
        <component :is="provider.icon" aria-hidden="true" :width="36" :height="36" />
      </span>
    </button>
  </div>
</template>
