<script setup lang="ts">
import { Copy } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'

defineProps<{
  maskedPrefix: string
  keyValue: string
}>()

const emit = defineEmits<{
  copy: [text: string]
}>()
</script>

<template>
  <div class="flex items-center gap-2">
    <code class="block min-w-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)">
      {{ maskedPrefix }}
    </code>
    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      label="复制完整密钥"
      @click="emit('copy', keyValue)"
    >
      <Copy class="size-3.5" />
    </BaseButton>
  </div>
</template>
