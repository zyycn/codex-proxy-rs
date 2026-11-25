<script setup lang="ts">
import type { Account, AccountConnectionUpdate, AccountGroupRef, AccountModelAccess } from '@/api'

import type { AccountConnectionForm } from '@/providers/accounts'

import { ZButton, ZDialog, ZForm } from '@codex-proxy/ui'
import { computed, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account-plan-badge/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { providerAccounts } from '@/providers/accounts'
import { useAccountSettingsFormRules } from '../composables/useAccountSettingsFormRules'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'

const props = defineProps<{
  account: Account | null
  groups: AccountGroupRef[]
  groupsLoading: boolean
  saving: boolean
}>()

const emit = defineEmits<{
  save: [connection?: AccountConnectionUpdate]
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const connectionComponent = computed(() => providerAccounts(props.account?.provider)?.connectionEditor)
const connection = useTemplateRef<AccountConnectionForm>('connectionEditor')
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
  connection: connection.value?.formModel,
  concurrencyLimit: concurrencyLimit.value,
  weight: weight.value,
  modelAccess: modelAccess.value,
  proxyMode: proxyMode.value,
  proxyId: proxyId.value,
}))
const rules = useAccountSettingsFormRules(() => form.value, {
  savedName: () => props.account?.name ?? '',
})
const saveDisabled = computed(() => !props.account || props.groupsLoading || (!!connectionComponent.value && !connection.value?.canSave))

watch(open, () => formRef.value?.clearValidate())

async function requestSave() {
  if (props.saving || saveDisabled.value || !await formRef.value?.validate().catch(() => false))
    return
  emit('save', connection.value?.change)
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('accounts.editor.title')"
    width="52rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
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
        :provider="account.provider"
      >
        <template #connection>
          <component
            :is="connectionComponent"
            v-if="connectionComponent"
            :key="account.id"
            ref="connectionEditor"
            :account="account"
            :open="open"
            :disabled="saving"
          />
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
