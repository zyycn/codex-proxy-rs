<script setup lang="ts">
import type { ApiKey } from '@/api'

import { ZIconButton, ZMenuItem } from '@codex-proxy/ui'

import { Pencil, Power, RotateCcw, Terminal, Trash2, Upload } from '@lucide/vue'

import { useI18n } from 'vue-i18n'
import RowActionMenu from '@/components/RowActionMenu.vue'

defineProps<{
  apiKey: ApiKey
  deleting: boolean
  updatingStatus: boolean
  revealing: boolean
  configuring: boolean
}>()

const emit = defineEmits<{
  use: [apiKey: ApiKey]
  importClient: [apiKey: ApiKey]
  toggle: [apiKey: ApiKey]
  delete: [apiKey: ApiKey]
  edit: [apiKey: ApiKey]
  resetBudget: [apiKey: ApiKey]
}>()

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div class="flex items-center justify-start gap-0.5">
    <ZIconButton
      size="small"
      :aria-label="t('keys.actions.editKey')"
      @click.stop="emit('edit', apiKey)"
    >
      <Pencil class="size-3.5 text-z-link" />
    </ZIconButton>
    <ZIconButton
      size="small"
      :aria-label="t('keys.actions.useKey')"
      :loading="configuring"
      :disabled="revealing"
      @click.stop="emit('use', apiKey)"
    >
      <Terminal class="size-3.5 text-z-primary-text" />
    </ZIconButton>

    <RowActionMenu :label="t('keys.actions.moreActions')" menu-class="w-36 p-1.5">
      <ZMenuItem @click.stop="emit('resetBudget', apiKey)">
        <template #icon>
          <RotateCcw class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('keys.actions.resetUsedCredit') }}
      </ZMenuItem>
      <ZMenuItem :disabled="revealing" @click.stop="emit('importClient', apiKey)">
        <template #icon>
          <Upload class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('keys.configuration.importTo') }}
      </ZMenuItem>
      <ZMenuItem :loading="updatingStatus" @click.stop="emit('toggle', apiKey)">
        <template #icon>
          <Power class="size-3.5" :class="apiKey.enabled ? 'text-z-warning' : 'text-z-success'" />
        </template>
        {{ apiKey.enabled ? t('keys.actions.disableKey') : t('keys.actions.enableKey') }}
      </ZMenuItem>
      <ZMenuItem type="danger" :disabled="deleting" @click.stop="emit('delete', apiKey)">
        <template #icon>
          <Trash2 class="size-3.5" />
        </template>
        {{ t('keys.actions.deleteKey') }}
      </ZMenuItem>
    </RowActionMenu>
  </div>
</template>
