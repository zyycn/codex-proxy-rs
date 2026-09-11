<script setup lang="ts">
import type { OutboundProxy } from '@/api'
import { Pencil, Trash2, Zap } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'

defineProps<{
  proxy: OutboundProxy
  testing: boolean
  deleting: boolean
}>()
const emit = defineEmits<{
  edit: [proxy: OutboundProxy]
  test: [proxy: OutboundProxy]
  delete: [proxy: OutboundProxy]
}>()
</script>

<template>
  <div class="flex items-center gap-1">
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="测试代理"
      :loading="testing"
      @click.stop="emit('test', proxy)"
    >
      <Zap class="size-3.5 text-cp-warning" />
    </BaseIconButton>
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="编辑代理"
      @click.stop="emit('edit', proxy)"
    >
      <Pencil class="size-3.5 text-cp-link" />
    </BaseIconButton>
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="删除代理"
      :disabled="deleting"
      @click.stop="emit('delete', proxy)"
    >
      <Trash2 class="size-3.5 text-cp-error" />
    </BaseIconButton>
  </div>
</template>
