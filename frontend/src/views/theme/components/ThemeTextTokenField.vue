<script setup lang="ts">
import { RotateCcw } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineProps<{
  label: string
  token: string
  value: string
  overridden: boolean
}>()

const emit = defineEmits<{
  change: [value: string]
  reset: []
}>()
</script>

<template>
  <div class="grid gap-2 rounded-cp-lg bg-cp-fill-quaternary px-3 py-3">
    <div class="flex items-center justify-between gap-3">
      <div class="min-w-0">
        <strong class="text-cp-sm font-bold text-cp-text">{{ label }}</strong>
        <code class="mt-1 block truncate font-mono text-[10px] text-cp-text-quaternary">{{ token }}</code>
      </div>
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
    <BaseInput
      :model-value="value"
      :aria-label="label"
      class="font-mono"
      @update:model-value="emit('change', $event)"
    />
  </div>
</template>
