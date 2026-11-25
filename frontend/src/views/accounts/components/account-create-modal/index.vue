<script setup lang="ts">
import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'

import type { AccountCreateForm } from '../../utils/accountCreate'
import type { Account, AccountGroupRef } from '@/api'
import type { AccountImportMode } from '@/providers/accounts'

import { ZButton, ZDialog, ZForm, ZIconButton, ZSegmented } from '@codex-proxy/ui'
import { Copy, LayoutGrid, Settings2 } from '@lucide/vue'
import { computed, useId, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account-plan-badge/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useCopyText } from '@/composables/useCopyText'
import { providerAccounts } from '@/providers/accounts'
import { providerIcon } from '@/providers/catalog'
import { useAccountSettingsFormRules } from '../../composables/useAccountSettingsFormRules'
import { accountCreateProvider } from '../../utils/accountCreate'
import AccountIdentityCell from '../AccountIdentityCell.vue'
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

const emit = defineEmits<{ create: [document?: Record<string, unknown>], generateOauth: [], afterLeave: [] }>()

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
const importFormId = useId()
const apiKeyForm = computed(() => providerAccounts(accountCreateProvider(form.value))?.apiKeyImportForm)
const view = computed(() => resolveAccountCreatePresentation({
  form: form.value,
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
      <component :is="providerIcon(view.provider)" v-else-if="view.provider" class="text-z-text" :width="20" :height="20" aria-hidden="true" />
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
          :panel-title="`${view.label} ${reauthorizing ? t('accounts.actions.reauthorize') : t('accounts.onboarding.authorize')}`"
          :panel-description="view.oauth.description"
          :loading="authorization.status === 'starting'"
          :callback-label="view.oauth.callbackLabel"
          :callback-placeholder="view.oauth.callbackPlaceholder"
          :disabled="busy || !view.provider"
          @regenerate="emit('generateOauth')"
        />
        <AccountImportFields
          v-else-if="mode !== 'api_key'"
          :key="mode"
          v-model="importText"
          :label="view.importInput.label"
          :placeholder="view.importInput.placeholder"
          :uploadable="view.importInput.uploadable"
          :disabled="busy"
        />
      </template>
      <!-- 草稿只在本次弹窗的同一 Provider 内跨步骤保留 -->
      <KeepAlive v-if="apiKeyForm" :key="view.provider" :max="1">
        <component
          :is="apiKeyForm"
          v-if="!view.configuring && mode === 'api_key'"
          :id="importFormId"
          :disabled="busy"
          @create="emit('create', $event)"
        />
      </KeepAlive>
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
      <ZButton v-else-if="mode === 'api_key'" type="primary" native-type="submit" :form="importFormId" :loading="saving" :disabled="!view.canSubmit">
        {{ view.submitLabel }}
      </ZButton>
      <ZButton v-else type="primary" :loading="saving || authorization.status === 'completing'" :disabled="!view.canSubmit" @click="emit('create')">
        {{ view.submitLabel }}
      </ZButton>
    </template>
  </ZDialog>
</template>
