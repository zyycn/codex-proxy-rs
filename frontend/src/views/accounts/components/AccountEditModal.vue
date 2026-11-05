<script setup lang="ts">
import type { FormRules } from '@codex-proxy/ui'
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'

import type { Account, AccountGroupRef, AccountModelAccess } from '@/api'

import { ZButton, ZDialog, ZForm, ZFormItem, ZSegmented } from '@codex-proxy/ui'

import { computed, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useAccountApiKeyFormRules } from '../composables/useAccountApiKeyFormRules'
import { useAccountSettingsFormRules } from '../composables/useAccountSettingsFormRules'
import { isOpenAiApiKeyAccount, isOpenAiOAuthAccount } from '../utils/upstreamApiKey'
import AccountApiKeyFields from './AccountApiKeyFields.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'

const props = defineProps<{
  account: Account | null
  groups: AccountGroupRef[]
  groupsLoading: boolean
  saving: boolean
  configurationReady: boolean
}>()

const emit = defineEmits<{
  save: []
  afterLeave: []
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const apiKey = defineModel<ApiKeyAccountForm>('apiKey', { required: true })
const oauthTransport = defineModel<ApiKeyAccountForm['transport']>('oauthTransport', { required: true })
const transportOptions = [
  { label: 'WS', value: 'prefer_websocket' },
  { label: 'SSE', value: 'http' },
]
const notes = defineModel<string>('notes', { required: true })
const name = defineModel<string>('name', { required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const form = computed(() => ({
  name: name.value,
  apiKey: apiKey.value,
  concurrencyLimit: concurrencyLimit.value,
  weight: weight.value,
  modelAccess: modelAccess.value,
  proxyMode: proxyMode.value,
  proxyId: proxyId.value,
}))
const settingsRules = useAccountSettingsFormRules(() => form.value, {
  savedName: () => props.account?.name ?? '',
})
const apiKeyRules = useAccountApiKeyFormRules(true)
const rules = computed<FormRules>(() => ({
  ...settingsRules.value,
  ...apiKeyRules.value,
}))
const saveDisabled = computed(() => !props.account || props.groupsLoading
  || (isOpenAiApiKeyAccount(props.account) && !props.configurationReady))

watch(open, () => formRef.value?.clearValidate())

async function requestSave() {
  if (props.saving || saveDisabled.value || !await formRef.value?.validate().catch(() => false))
    return
  emit('save')
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('accounts.editor.title')"
    width="42rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
    @closed="emit('afterLeave')"
  >
    <ZForm v-if="account" ref="formElement" :model="form" :rules="rules" :validate-on-rule-change="false" scroll-to-error focus-on-error novalidate @submit="requestSave">
      <div
        class="flex flex-wrap items-center justify-between gap-4 rounded-z bg-z-fill-quaternary px-4 py-3.5"
      >
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="lg"
          show-notes
        />
        <div class="flex shrink-0 items-center gap-3">
          <AccountPlanBadge :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
        </div>
      </div>

      <AccountApiKeyFields v-if="isOpenAiApiKeyAccount(account)" v-model="apiKey" editing :disabled="saving || !configurationReady" />

      <AccountSettingsFields
        v-model:name="name"
        v-model:notes="notes"
        v-model:enabled="enabled"
        v-model:concurrency-limit="concurrencyLimit"
        v-model:weight="weight"
        v-model:model-access="modelAccess"
        v-model:selected-group-ids="selectedGroupIds"
        v-model:proxy-mode="proxyMode"
        v-model:proxy-id="proxyId"
        :groups="groups"
        :groups-loading="groupsLoading"
        :disabled="saving"
        :show-scheduling="false"
        :current-proxy-id="account.outboundProxyId"
        :endpoint="account.outboundProxyEndpoint"
        :account-id="account.id"
      >
        <template v-if="isOpenAiOAuthAccount(account)" #name-extra>
          <ZFormItem :label="t('accounts.editor.transportLabel')">
            <ZSegmented v-model="oauthTransport" class="w-full" :aria-label="t('accounts.editor.transportLabel')" :options="transportOptions" :disabled="saving || !configurationReady" />
          </ZFormItem>
        </template>
      </AccountSettingsFields>
    </ZForm>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="saveDisabled"
        @click="requestSave"
      >
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
