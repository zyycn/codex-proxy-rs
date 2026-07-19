<script setup lang="ts">
import type { getApiKeys } from '@/api'
import { Power, Terminal, Trash2, Upload } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'

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
}>()
</script>

<template>
  <div class="flex items-center justify-start gap-1">
    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      label="使用密钥"
      :loading="revealing"
      :disabled="revealing"
      @click.stop="emit('use', apiKey)"
    >
      <Terminal class="size-3.5 text-(--cp-normal)" />
    </BaseButton>

    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      label="导入 CCSwitch"
      :disabled="revealing"
      @click.stop="emit('importCcs', apiKey)"
    >
      <Upload class="size-3.5 text-(--cp-info)" />
    </BaseButton>

    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      :label="apiKey.enabled ? '禁用密钥' : '启用密钥'"
      :loading="updatingStatus"
      @click.stop="emit('toggle', apiKey)"
    >
      <Power
        class="size-3.5"
        :class="apiKey.enabled ? 'text-(--cp-warning)' : 'text-(--cp-success)'"
      />
    </BaseButton>

    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      label="删除密钥"
      :disabled="deleting"
      @click.stop="emit('delete', apiKey)"
    >
      <Trash2 class="size-3.5 text-(--cp-danger)" />
    </BaseButton>
  </div>
</template>
