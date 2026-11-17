<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'

import { ZIconButton, ZMenuItem } from '@codex-proxy/ui'
import { Pencil, Power, Trash2, Wifi } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import RowActionMenu from '@/components/RowActionMenu.vue'

defineProps<{
  proxy: OutboundProxyRecord
  testing: boolean
  batchTesting: boolean
  updating: boolean
}>()

const emit = defineEmits<{
  test: [proxy: OutboundProxyRecord]
  edit: [proxy: OutboundProxyRecord]
  toggle: [proxy: OutboundProxyRecord]
  delete: [proxy: OutboundProxyRecord]
}>()

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div class="flex items-center gap-0.5">
    <ZIconButton size="small" :aria-label="t('proxies.actions.test')" :loading="testing && !batchTesting" :disabled="batchTesting || testing || updating" @click="emit('test', proxy)">
      <Wifi class="size-3.5 text-z-link" />
    </ZIconButton>
    <ZIconButton size="small" :aria-label="t('proxies.actions.edit')" :disabled="testing || updating" @click="emit('edit', proxy)">
      <Pencil class="size-3.5 text-z-link" />
    </ZIconButton>
    <RowActionMenu :label="t('proxies.actions.moreActionsFor', { name: proxy.name })" menu-class="w-36 p-1.5" :disabled="testing || updating" :loading="updating">
      <ZMenuItem :disabled="testing || updating" @click="emit('toggle', proxy)">
        <template #icon>
          <Power class="size-3.5" :class="proxy.enabled ? 'text-z-warning' : 'text-z-success'" />
        </template>
        {{ proxy.enabled ? t('proxies.actions.disableProxy') : t('proxies.actions.enableProxy') }}
      </ZMenuItem>
      <ZMenuItem type="danger" :disabled="testing || updating" @click="emit('delete', proxy)">
        <template #icon>
          <Trash2 class="size-3.5" />
        </template>
        {{ t('proxies.actions.deleteProxy') }}
      </ZMenuItem>
    </RowActionMenu>
  </div>
</template>
