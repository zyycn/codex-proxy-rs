<script setup lang="ts">
import { BaseIconButton } from '@codex-proxy/ui'

import { Copy } from '@lucide/vue'

defineProps<{
  prefix: string
  revealing: boolean
}>()

const emit = defineEmits<{
  copy: []
}>()
</script>

<template>
  <div class="flex items-center gap-2">
    <code class="block min-w-0 overflow-hidden whitespace-nowrap font-mono text-cp-sm font-emphasis text-cp-text">
      {{ prefix }}••••••••••••••••
    </code>
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="复制完整密钥"
      :loading="revealing"
      :disabled="revealing"
      @click="emit('copy')"
    >
      <Copy class="size-3.5" />
    </BaseIconButton>
  </div>
</template>
