<script setup lang="ts">
import type { AccountRow } from '../../constants'
import type { AccountCreateForm, AccountImportMode } from './model'
import type { AccountGroup } from '@/api'
import { Openai, Xai } from '@boxicons/vue'
import { LayoutGrid, Settings2 } from '@lucide/vue'
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { parseAccountSchedulingForm } from '../../utils/schedulingForm'
import AccountImportFields from './AccountImportFields.vue'
import AccountOAuthFields from './AccountOAuthFields.vue'
import AccountSetupFields from './AccountSetupFields.vue'
import { accountProxyError } from './model'
import { resolveAccountCreatePresentation } from './presenter'

const props = withDefaults(defineProps<{
  groups: AccountGroup[]
  groupsLoading: boolean
  saving?: boolean
  oauthLoading?: boolean
  reauthorizing?: boolean
  account?: AccountRow | null
}>(), { saving: false, oauthLoading: false, reauthorizing: false, account: null })

const emit = defineEmits<{ create: [], generateOauth: [] }>()
const open = defineModel<boolean>({ default: false })
const form = defineModel<AccountCreateForm>('form', { required: true })
const busy = computed(() => props.saving || props.oauthLoading)
const proxyError = computed(() => accountProxyError(form.value))
const scheduling = computed(() => parseAccountSchedulingForm(form.value.concurrencyLimit, form.value.weight))
const view = computed(() => resolveAccountCreatePresentation({
  form: form.value,
  account: props.account,
  saving: props.saving,
  oauthLoading: props.oauthLoading,
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
  get: () => form.value.mode === 'oauth' ? '' : form.value.importTexts[form.value.mode],
  set: (value: string) => {
    if (form.value.mode !== 'oauth')
      form.value.importTexts[form.value.mode] = value
  },
})

function continueToImport() {
  if (form.value.provider && scheduling.value.valid && !props.groupsLoading && !proxyError.value && !busy.value)
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
      <Xai v-else-if="view.isXai" class="text-cp-text" :width="20" :height="20" aria-hidden="true" />
      <Openai v-else class="text-cp-text" :width="20" :height="20" aria-hidden="true" />
    </template>

    <div class="grid gap-4">
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
          v-model="form.oauthCallback"
          :auth-url="view.oauth.authUrl"
          :panel-title="`${view.isXai ? 'xAI' : 'OpenAI'} OAuth ${reauthorizing ? '重新授权' : '授权'}`"
          :panel-description="view.isXai ? '生成并打开授权链接、完成浏览器授权、粘贴回调地址或授权码' : '生成并打开授权链接、完成浏览器授权、粘贴回调地址'"
          :loading="oauthLoading"
          :callback-label="view.oauth.callbackLabel"
          :callback-placeholder="view.oauth.callbackPlaceholder"
          :disabled="busy"
          @regenerate="emit('generateOauth')"
        />
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
      <BaseButton v-if="view.configuring" variant="primary" :disabled="!form.provider || !scheduling.valid || groupsLoading || Boolean(proxyError) || busy" @click="continueToImport">
        继续导入
      </BaseButton>
      <BaseButton v-else variant="primary" :loading="saving || oauthLoading" :disabled="!view.canSubmit" @click="emit('create')">
        {{ view.submitLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
