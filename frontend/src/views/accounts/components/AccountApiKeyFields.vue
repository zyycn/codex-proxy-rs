<script setup lang="ts">
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'

import { ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled?: boolean, editing?: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const form = defineModel<ApiKeyAccountForm>({ required: true })
const transportOptions = computed(() => ([
  { label: 'HTTP / SSE', value: 'http' },
  {
    label: t('accounts.credentials.wsFirst'),
    value: 'prefer_websocket',
  },
]))
</script>

<template>
  <div class="grid gap-4">
    <ZFormItem v-if="!editing" :label="t('accounts.credentials.accountName')" required>
      <ZInput v-model="form.name" :placeholder="t('accounts.credentials.enterUpstreamName')" :disabled="disabled" />
    </ZFormItem>
    <ZFormItem :label="t('common.fields.baseUrl')" required>
      <template #extra>
        <span class="text-cp-xs text-cp-text-quaternary" :title="t('accounts.credentials.baseUrlHelp')">{{ t('accounts.credentials.apiPrefix') }}</span>
      </template>
      <ZInput v-model="form.base_url" type="url" placeholder="https://api.example.com/v1" :disabled="disabled" autocomplete="off" />
    </ZFormItem>
    <ZFormItem :label="t('common.fields.apiKey')" :required="!editing">
      <ZInput v-model="form.apiKey" type="password" :placeholder="editing ? t('accounts.credentials.apiKeyPlaceholder') : t('accounts.credentials.enterUpstreamKey')" :disabled="disabled" autocomplete="new-password" />
    </ZFormItem>
    <ZFormItem :label="t('accounts.credentials.transportLabel')">
      <ZSegmented v-model="form.transport" :aria-label="t('accounts.credentials.transportLabel')" :options="transportOptions" :disabled="disabled" :title="t('accounts.credentials.webSocketHelp')" />
    </ZFormItem>
  </div>
</template>
