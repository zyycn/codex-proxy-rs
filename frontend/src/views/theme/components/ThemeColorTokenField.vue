<script setup lang="ts">
import { RotateCcw } from '@lucide/vue'

import BaseColorPicker from '@/components/base/BaseColorPicker/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'

withDefaults(defineProps<{
  label: string
  token: string
  value: string
  description?: string
  presets?: readonly string[]
  overridden?: boolean
}>(), {
  description: undefined,
  presets: () => [],
  overridden: false,
})

const emit = defineEmits<{
  change: [value: string]
  reset: []
}>()
</script>

<template>
  <div class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-cp-lg bg-cp-fill-quaternary px-3 py-3">
    <div class="min-w-0">
      <div class="flex flex-wrap items-center gap-2">
        <strong class="text-cp-sm font-bold text-cp-text">{{ label }}</strong>
        <span v-if="overridden" class="rounded-full bg-cp-primary-bg px-1.5 py-0.5 text-[9px] font-heavy text-cp-primary-text">
          已修改
        </span>
      </div>
      <code class="mt-1 block truncate font-mono text-[10px] text-cp-text-quaternary">{{ token }}</code>
      <p
        v-if="description"
        class="mt-1.5 mb-0 truncate text-[10px] leading-[1.45] font-emphasis text-cp-text-secondary"
        :title="description"
      >
        {{ description }}
      </p>
    </div>

    <div class="flex items-center gap-1.5 rounded-cp bg-cp-bg-container p-1 shadow-cp-tertiary">
      <BaseColorPicker
        :model-value="value"
        :presets="presets"
        :allow-alpha="false"
        :label="`编辑 ${label}`"
        @update:model-value="emit('change', $event)"
      />
      <span class="min-w-17 font-mono text-[10px] tabular-nums text-cp-text-secondary">{{ value }}</span>
      <BaseIconButton
        v-if="overridden"
        label="恢复派生值"
        size="sm"
        variant="ghost"
        @click="emit('reset')"
      >
        <RotateCcw class="size-3.5" />
      </BaseIconButton>
    </div>
  </div>
</template>
