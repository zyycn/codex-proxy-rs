<script setup lang="ts">
import type { getApiKeys } from '@/api'
import { MoreHorizontal, Pencil, Power, RotateCcw, Terminal, Trash2, Upload } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BasePopover from '@/components/base/BasePopover.vue'

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
  resetBudget: [apiKey: ApiKeyRow]
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

    <BasePopover placement="bottom-end">
      <template #trigger="{ open }">
        <BaseIconButton variant="ghost" size="sm" label="更多操作" :pressed="open">
          <MoreHorizontal class="size-4" />
        </BaseIconButton>
      </template>
      <template #default="{ close }">
        <div class="w-44 p-1.5">
          <BaseMenuItem @click.stop="(close(), emit('resetBudget', apiKey))">
            <template #icon>
              <RotateCcw class="size-3.5 text-cp-text-quaternary" />
            </template>
            重置已用额度
          </BaseMenuItem>
          <BaseMenuItem :disabled="revealing" @click.stop="(close(), emit('importCcs', apiKey))">
            <template #icon>
              <Upload class="size-3.5 text-cp-text-quaternary" />
            </template>
            导入 CCSwitch
          </BaseMenuItem>
          <BaseMenuItem :loading="updatingStatus" @click.stop="(close(), emit('toggle', apiKey))">
            <template #icon>
              <Power class="size-3.5" :class="apiKey.enabled ? 'text-cp-warning' : 'text-cp-success'" />
            </template>
            {{ apiKey.enabled ? '禁用密钥' : '启用密钥' }}
          </BaseMenuItem>
          <BaseMenuItem tone="destructive" :disabled="deleting" @click.stop="(close(), emit('delete', apiKey))">
            <template #icon>
              <Trash2 class="size-3.5" />
            </template>
            删除密钥
          </BaseMenuItem>
        </div>
      </template>
    </BasePopover>
  </div>
</template>
