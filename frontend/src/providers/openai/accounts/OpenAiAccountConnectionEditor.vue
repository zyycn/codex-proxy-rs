<script setup lang="ts">
import type { Account } from '@/api'
import { ZFormItem, ZSegmented } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import ApiKeyFields from './ApiKeyFields.vue'
import { useConnectionEditor } from './useConnectionEditor'

const props = defineProps<{ account: Account, open: boolean, disabled: boolean }>()
const { apiKey, oauthTransport, kind, configurationReady, canSave, change } = useConnectionEditor(() => props.account, () => props.open)
defineExpose({ formModel: apiKey, canSave, change })
const { t } = useI18n({ useScope: 'global' })
const transportOptions = [
  { label: 'WS', value: 'prefer_websocket' },
  { label: 'SSE', value: 'http' },
]
</script>

<template>
  <ApiKeyFields v-if="kind === 'api_key'" v-model="apiKey" editing :disabled="disabled || !configurationReady" />
  <ZFormItem v-else-if="kind === 'oauth'" :label="t('accounts.editor.transportLabel')">
    <ZSegmented v-model="oauthTransport" class="w-40" :aria-label="t('accounts.editor.transportLabel')" :options="transportOptions" :disabled="disabled || !configurationReady" />
  </ZFormItem>
</template>
