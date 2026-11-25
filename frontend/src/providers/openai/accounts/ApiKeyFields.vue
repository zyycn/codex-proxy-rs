<script setup lang="ts">
import type { ApiKeyAccountForm } from './connection'

import { ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import { useApiKeyFormRules } from './useApiKeyFormRules'

const props = defineProps<{ disabled?: boolean, editing?: boolean }>()
const rules = useApiKeyFormRules(!!props.editing)

const { t } = useI18n({ useScope: 'global' })

const form = defineModel<ApiKeyAccountForm>({ required: true })
const protocolOptions = [
  { label: 'Responses', value: 'responses' },
  { label: 'Messages', value: 'messages' },
]
const transportOptions = computed(() => [
  { label: 'SSE', value: 'http' },
  { label: 'WS', value: 'prefer_websocket', disabled: form.value.protocol === 'messages' },
])

function updateProtocol(protocol: string) {
  if (protocol !== 'responses' && protocol !== 'messages')
    return
  form.value = {
    ...form.value,
    protocol,
    transport: protocol === 'messages'
      ? 'http'
      : form.value.transport,
  }
}
</script>

<template>
  <ZFormItem prop="connection.base_url" :rules="rules.base_url" :show-message="Boolean(form.base_url.trim())" :label="t('common.fields.baseUrl')" required>
    <template #extra>
      <span class="text-z-xs text-z-text-quaternary" :title="t('accounts.credentials.baseUrlHelp')">{{ t('accounts.credentials.apiPrefix') }}</span>
    </template>
    <ZInput v-model="form.base_url" type="url" placeholder="https://api.example.com/v1" :disabled="disabled" autocomplete="off" />
  </ZFormItem>
  <ZFormItem prop="connection.apiKey" :rules="rules.apiKey" :show-message="Boolean(form.apiKey)" :label="t('common.fields.apiKey')" :required="!editing">
    <ZInput v-model="form.apiKey" type="password" :placeholder="editing ? t('accounts.credentials.apiKeyPlaceholder') : t('accounts.credentials.enterUpstreamKey')" :disabled="disabled" autocomplete="new-password" />
  </ZFormItem>
  <div class="grid grid-cols-[minmax(0,1fr)_auto] gap-4 sm:grid-cols-2">
    <ZFormItem prop="connection.protocol" :rules="rules.protocol" :show-message="false" :label="t('accounts.credentials.protocolLabel')">
      <ZSegmented :model-value="form.protocol" :aria-label="t('accounts.credentials.protocolLabel')" :options="protocolOptions" :disabled="disabled" @update:model-value="updateProtocol" />
    </ZFormItem>
    <ZFormItem prop="connection.transport" :rules="rules.transport" :show-message="false" :label="t('accounts.credentials.transportLabel')">
      <ZSegmented v-model="form.transport" :aria-label="t('accounts.credentials.transportLabel')" :options="transportOptions" :disabled="disabled" :title="form.protocol === 'messages' ? t('accounts.credentials.messagesTransportHelp') : t('accounts.credentials.webSocketHelp')" />
    </ZFormItem>
  </div>
</template>
