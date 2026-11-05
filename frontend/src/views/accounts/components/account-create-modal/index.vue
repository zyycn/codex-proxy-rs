<script setup lang="ts">
import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'

import type { AccountCreateForm, AccountImportMode } from '../../utils/accountCreate'
import type { Account, AccountGroupRef } from '@/api'

import { Openai, Xai } from '@boxicons/vue'
import { ZButton, ZDialog, ZForm, ZIconButton, ZSegmented } from '@codex-proxy/ui'
import { Copy, LayoutGrid, Settings2 } from '@lucide/vue'
import { computed, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useCopyText } from '@/composables/useCopyText'
import { useAccountApiKeyFormRules } from '../../composables/useAccountApiKeyFormRules'
import { useAccountSettingsFormRules } from '../../composables/useAccountSettingsFormRules'
import { accountCreateProvider } from '../../utils/accountCreate'
import AccountApiKeyFields from '../AccountApiKeyFields.vue'
import AccountIdentityCell from '../AccountIdentityCell.vue'
import AccountNameField from '../AccountNameField.vue'
import AccountImportFields from './AccountImportFields.vue'
import AccountOAuthFields from './AccountOAuthFields.vue'
import AccountSetupFields from './AccountSetupFields.vue'
import { resolveAccountCreatePresentation } from './presenter'

const props = withDefaults(defineProps<{
  groups: AccountGroupRef[]
  groupsLoading: boolean
  saving?: boolean
  reauthorizing?: boolean
  account?: Account | null
  authorization: AccountAuthorizationView
}>(), {
  saving: false,
  reauthorizing: false,
  account: null,
})

const emit = defineEmits<{ create: [], generateOauth: [], afterLeave: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const form = defineModel<AccountCreateForm>('form', { required: true })
const callback = defineModel<string>('callback', { required: true })
const copyWithToast = useCopyText()
const accountCopyValue = computed(() =>
  props.account?.email?.trim()
  || props.account?.accountId?.trim()
  || props.account?.id
  || '',
)
const busy = computed(() => props.saving
  || props.authorization.status === 'starting'
  || props.authorization.status === 'completing')
const canSelect = computed(() => form.value.source?.kind === 'bundle' || Boolean(accountCreateProvider(form.value)))
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const settingsRules = useAccountSettingsFormRules(() => ({
  ...form.value,
  name: form.value.source?.kind === 'provider'
    ? form.value.name
    : undefined,
}))
const apiKeyRules = useAccountApiKeyFormRules()
const view = computed(() => resolveAccountCreatePresentation({
  form: form.value,
  provider: accountCreateProvider(form.value),
  authorization: props.authorization,
  callback: callback.value,
  busy: busy.value,
  reauthorizing: props.reauthorizing,
}))
const mode = computed({
  get: () => form.value.mode,
  set: (value: string) => {
    if (!props.reauthorizing && view.value.modeOptions.some(option => option.value === value))
      form.value.mode = value as AccountImportMode
  },
})
const importText = computed({
  get: () => form.value.mode === 'oauth' || form.value.mode === 'api_key'
    ? ''
    : form.value.importTexts[form.value.mode],
  set: (value: string) => {
    if (form.value.mode !== 'oauth' && form.value.mode !== 'api_key')
      form.value.importTexts[form.value.mode] = value
  },
})

watch([
  open,
  () => form.value.step,
  mode,
], () => formRef.value?.clearValidate())

async function continueToImport() {
  if (!canSelect.value || props.groupsLoading || busy.value || !await formRef.value?.validate().catch(() => false))
    return
  form.value.step = 'import'
}

async function requestCreate() {
  if (!view.value.canSubmit)
    return
  if (mode.value === 'api_key' && !await formRef.value?.validate().catch(() => false))
    return
  emit('create')
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="view.modal.title"
    :description="view.modal.description"
    :type="view.modal.type"
    :width="view.modal.width"
    :show-close="!busy" :close-on-click-modal="!busy" :close-on-press-escape="!busy"
    @closed="emit('afterLeave')"
  >
    <template #icon>
      <Settings2 v-if="view.configuring" class="text-z-text" :size="20" aria-hidden="true" />
      <LayoutGrid v-else-if="view.isBatch" class="text-z-text" :size="20" aria-hidden="true" />
      <Xai v-else-if="view.provider === 'xai'" class="text-z-text" :width="20" :height="20" aria-hidden="true" />
      <Openai v-else-if="view.provider === 'openai'" class="text-z-text" :width="20" :height="20" aria-hidden="true" />
    </template>

    <div class="grid gap-4">
      <div
        v-if="reauthorizing && account"
        class="flex flex-wrap items-center justify-between gap-4 rounded-z bg-z-fill-quaternary px-4 py-3.5"
      >
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="lg"
        />
        <div class="flex shrink-0 items-center gap-3">
          <AccountPlanBadge
            :upstream="account.upstream"
            :authentication-kind="account.authenticationKind"
            :plan-type="account.planType"
            :plan-type-display="account.planTypeDisplay"
            size="sm"
          />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
          <ZIconButton
            variant="solid"
            size="small"
            :aria-label="t('accounts.onboarding.copyAccount')"
            :disabled="busy || !accountCopyValue"
            @click="copyWithToast(accountCopyValue, { successText: t('accounts.onboarding.accountCopied') })"
          >
            <Copy class="size-3.5" />
          </ZIconButton>
        </div>
      </div>

      <ZForm v-if="view.configuring" ref="formElement" :model="form" :rules="settingsRules" :validate-on-rule-change="false" scroll-to-error focus-on-error novalidate @submit="continueToImport">
        <AccountSetupFields v-model="form" :disabled="busy" :groups="groups" :groups-loading="groupsLoading" />
      </ZForm>
      <template v-if="!view.configuring">
        <ZSegmented
          v-if="!reauthorizing && !view.isBatch"
          v-model="mode"
          :aria-label="t('accounts.onboarding.howToAddAccount')"
          :options="view.modeOptions"
          :disabled="busy"
          class="w-full"
        />
        <AccountOAuthFields
          v-if="mode === 'oauth'"
          v-model="callback"
          :auth-url="authorization.flow?.authorizationUrl ?? ''"
          :authorization="authorization"
          :can-start="true"
          :panel-title="`${view.label} ${reauthorizing ? t('accounts.actions.reauthorize') : t('accounts.onboarding.authorize')}`"
          :panel-description="view.oauth.description"
          :loading="authorization.status === 'starting'"
          :callback-label="view.oauth.callbackLabel"
          :callback-placeholder="view.oauth.callbackPlaceholder"
          :disabled="busy"
          @regenerate="emit('generateOauth')"
        />
        <ZForm v-else-if="mode === 'api_key'" ref="formElement" :model="form" :rules="apiKeyRules" scroll-to-error focus-on-error novalidate @submit="requestCreate">
          <AccountNameField v-model="form.name" required :disabled="busy" :placeholder="t('accounts.credentials.enterUpstreamName')" />
          <AccountApiKeyFields v-model="form.apiKey" :disabled="busy" />
        </ZForm>
        <AccountImportFields
          v-else
          :key="mode"
          v-model="importText"
          :label="view.importInput.label"
          :placeholder="view.importInput.placeholder"
          :uploadable="view.importInput.uploadable"
          :disabled="busy"
        />
      </template>
    </div>

    <template #footer>
      <ZButton v-if="view.configuring || reauthorizing" :disabled="busy" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton v-else class="mr-auto" :disabled="busy" @click="form.step = 'settings'">
        {{ t('common.actions.previous') }}
      </ZButton>
      <ZButton v-if="view.configuring" type="primary" :disabled="!canSelect || groupsLoading || busy" @click="continueToImport">
        {{ t('accounts.onboarding.continueImporting') }}
      </ZButton>
      <ZButton v-else type="primary" :loading="saving || authorization.status === 'completing'" :disabled="!view.canSubmit" @click="requestCreate">
        {{ view.submitLabel }}
      </ZButton>
    </template>
  </ZDialog>
</template>
