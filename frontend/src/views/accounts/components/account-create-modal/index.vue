<script setup lang="ts">
import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'
import type { AccountCreateForm, AccountImportMode } from '../../utils/accountCreate'

import type { Account, AccountGroup } from '@/api'
import { Openai, Xai } from '@boxicons/vue'
import { BaseButton, BaseIconButton, BaseModal, BaseSegmented } from '@codex-proxy/ui'
import { Copy, LayoutGrid, Settings2 } from '@lucide/vue'
import { computed } from 'vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useCopyText } from '@/composables/useCopyText'
import { accountCreateProvider, accountProxyError } from '../../utils/accountCreate'
import { accountModelAccessError } from '../../utils/modelAccess'
import { parseAccountSchedulingForm } from '../../utils/schedulingForm'
import AccountApiKeyFields from '../AccountApiKeyFields.vue'
import AccountIdentityCell from '../AccountIdentityCell.vue'
import AccountImportFields from './AccountImportFields.vue'
import AccountOAuthFields from './AccountOAuthFields.vue'
import AccountSetupFields from './AccountSetupFields.vue'
import { resolveAccountCreatePresentation } from './presenter'

const props = withDefaults(defineProps<{
  groups: AccountGroup[]
  groupsLoading: boolean
  saving?: boolean
  oauthLoading?: boolean
  reauthorizing?: boolean
  account?: Account | null
  authorization: AccountAuthorizationView
}>(), { saving: false, oauthLoading: false, reauthorizing: false, account: null })

const emit = defineEmits<{ create: [], generateOauth: [] }>()
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
const busy = computed(() => props.saving || props.oauthLoading)
const canSelect = computed(() => form.value.source?.kind === 'bundle' || Boolean(accountCreateProvider(form.value)))
const proxyError = computed(() => accountProxyError(form.value))
const modelError = computed(() => accountModelAccessError(form.value.modelAccess))
const scheduling = computed(() => parseAccountSchedulingForm(form.value.concurrencyLimit, form.value.weight))
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
  get: () => form.value.mode === 'oauth' || form.value.mode === 'api_key' ? '' : form.value.importTexts[form.value.mode],
  set: (value: string) => {
    if (form.value.mode !== 'oauth' && form.value.mode !== 'api_key')
      form.value.importTexts[form.value.mode] = value
  },
})

function continueToImport() {
  if (canSelect.value && !modelError.value && scheduling.value.valid && !props.groupsLoading && !proxyError.value && !busy.value)
    form.value.step = 'import'
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="view.modal.title"
    :description="view.modal.description"
    :tone="view.modal.tone"
    :size="view.modal.size"
    :dismissible="!busy"
  >
    <template #icon>
      <Settings2 v-if="view.configuring" class="text-cp-text" :size="20" aria-hidden="true" />
      <LayoutGrid v-else-if="view.isBatch" class="text-cp-text" :size="20" aria-hidden="true" />
      <Xai v-else-if="view.provider === 'xai'" class="text-cp-text" :width="20" :height="20" aria-hidden="true" />
      <Openai v-else-if="view.provider === 'openai'" class="text-cp-text" :width="20" :height="20" aria-hidden="true" />
    </template>

    <div class="grid gap-4">
      <div
        v-if="reauthorizing && account"
        class="flex flex-wrap items-center justify-between gap-4 rounded-cp bg-cp-fill-quaternary px-4 py-3.5"
      >
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="lg"
        />
        <div class="flex shrink-0 items-center gap-3">
          <AccountPlanBadge
            :plan-type="account.planType"
            :plan-type-display="account.planTypeDisplay"
            size="sm"
          />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
          <BaseIconButton
            variant="secondary"
            size="sm"
            label="复制账号"
            :disabled="busy || !accountCopyValue"
            @click="copyWithToast(accountCopyValue, { successText: '账号已复制' })"
          >
            <Copy class="size-3.5" />
          </BaseIconButton>
        </div>
      </div>

      <AccountSetupFields
        v-if="view.configuring"
        v-model="form"
        :disabled="busy"
        :groups="groups"
        :groups-loading="groupsLoading"
        :proxy-error="form.proxyId.trim() ? proxyError : undefined"
      />
      <p v-if="view.configuring && !scheduling.valid" class="m-0 text-xs text-cp-error" role="alert">
        {{ scheduling.message }}
      </p>
      <template v-if="!view.configuring">
        <BaseSegmented
          v-if="!reauthorizing && !view.isBatch"
          v-model="mode"
          label="账号添加方式"
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
          :panel-title="`${view.label} ${reauthorizing ? '重新授权' : '授权'}`"
          :panel-description="view.oauth.description"
          :loading="oauthLoading"
          :callback-label="view.oauth.callbackLabel"
          :callback-placeholder="view.oauth.callbackPlaceholder"
          :disabled="busy"
          @regenerate="emit('generateOauth')"
        />
        <AccountApiKeyFields v-else-if="mode === 'api_key'" v-model="form.apiKey" :disabled="busy" />
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
      <BaseButton v-if="view.configuring || reauthorizing" variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton v-else class="mr-auto" variant="secondary" :disabled="busy" @click="form.step = 'settings'">
        上一步
      </BaseButton>
      <BaseButton v-if="view.configuring" variant="primary" :disabled="!canSelect || !scheduling.valid || Boolean(modelError) || groupsLoading || Boolean(proxyError) || busy" @click="continueToImport">
        继续导入
      </BaseButton>
      <BaseButton v-else variant="primary" :loading="saving || oauthLoading" :disabled="!view.canSubmit" @click="emit('create')">
        {{ view.submitLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
