<script setup lang="ts">
import type { getApiKeys } from '@/api'
import { Pencil, Power, Terminal, Trash2, Upload } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

defineProps<{
  apiKey: ApiKeyRow
  deleting: boolean
  updatingStatus: boolean
  revealing: boolean
}>()

const emit = defineEmits<{
  use: [apiKey: ApiKeyRow]
  importCcs: [apiKey: ApiKeyRow]
  toggle: [apiKey: ApiKeyRow]
  delete: [apiKey: ApiKeyRow]
  edit: [apiKey: ApiKeyRow]
}>()
</script>

<template>
  <div class="flex items-center justify-start gap-0.5">
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="编辑密钥"
      @click.stop="emit('edit', apiKey)"
    >
      <Pencil class="size-3.5 text-cp-link" />
    </BaseIconButton>
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="使用密钥"
      :loading="revealing"
      :disabled="revealing"
      @click.stop="emit('use', apiKey)"
    >
      <Terminal class="size-3.5 text-cp-primary-text" />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      label="导入 CCSwitch"
      :disabled="revealing"
      @click.stop="emit('importCcs', apiKey)"
    >
      <Upload class="size-3.5 text-cp-link" />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      :label="apiKey.enabled ? '禁用密钥' : '启用密钥'"
      :loading="updatingStatus"
      @click.stop="emit('toggle', apiKey)"
    >
      <Power
        class="size-3.5"
        :class="apiKey.enabled ? 'text-cp-warning' : 'text-cp-success'"
      />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      label="删除密钥"
      :disabled="deleting"
      @click.stop="emit('delete', apiKey)"
    >
      <Trash2 class="size-3.5 text-cp-error" />
    </BaseIconButton>
  </div>
</template>
