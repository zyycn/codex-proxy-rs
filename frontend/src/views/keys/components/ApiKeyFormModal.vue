<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyEditor'

import type { AccountGroupRef, ApiKeyPeerSharing } from '@/api'
import type { BuiltinProvider } from '@/providers/catalog'
import { ZButton, ZDialog, ZForm, ZFormItem, ZInput } from '@codex-proxy/ui'
import { KeyRound } from '@lucide/vue'

import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import ProviderSettingsSection from '@/components/ProviderSettingsSection.vue'
import ProviderRequestProfilesEditor from '@/components/request-profile/index.vue'
import { isValidInstallationSeed } from '@/providers/openai/request-profile/installationSeed'
import InstallationSeedField from '@/providers/openai/request-profile/InstallationSeedField.vue'

import { useApiKeyFormRules } from '../composables/useApiKeyFormRules'
import ApiKeyLimitFields from './ApiKeyLimitFields.vue'
import ApiKeyPeerSharingField from './ApiKeyPeerSharingField.vue'

const props = defineProps<{
  groups: AccountGroupRef[]
  groupLoading: boolean
  editing: boolean
  saving: boolean
}>()

const emit = defineEmits<{
  save: []
  afterLeave: []
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const form = defineModel<ApiKeyFormValue>('form', { required: true })
const provider = shallowRef<BuiltinProvider>('openai')
const seedField = useTemplateRef<InstanceType<typeof InstallationSeedField>>('seedField')
const title = computed(() => props.editing
  ? t('keys.actions.editKey')
  : t('keys.filters.createApiKey'))
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const rules = useApiKeyFormRules(() => form.value)

const installationSeedError = computed(() => isValidInstallationSeed(form.value.installationSeed)
  ? ''
  : t('settings.clientProfile.invalidInstallationSeed'))

watch(open, (visible) => {
  formRef.value?.clearValidate()
  if (visible)
    provider.value = 'openai'
})

async function requestSave() {
  if (props.saving)
    return
  if (installationSeedError.value) {
    provider.value = 'openai'
    await nextTick()
    seedField.value?.focus()
    return
  }
  if (!await formRef.value?.validate().catch(() => false))
    return
  emit('save')
}

function updateForm<Key extends keyof ApiKeyFormValue>(key: Key, value: ApiKeyFormValue[Key]) {
  form.value = { ...form.value, [key]: value }
}

function updatePeerSharing(key: keyof ApiKeyPeerSharing, value: boolean) {
  const sharing = { ...form.value.peerSharing, [key]: value }
  if (!sharing.sharePool)
    sharing.sharePlans = false
  updateForm('peerSharing', sharing)
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="title"
    :description="t('keys.editor.description')"
    type="info"
    width="48rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
    @closed="emit('afterLeave')"
  >
    <template #icon>
      <KeyRound class="text-z-text" :size="20" aria-hidden="true" />
    </template>

    <ZForm ref="formElement" :model="form" :rules="rules" scroll-to-error focus-on-error novalidate class="grid gap-6" @submit="requestSave">
      <div class="grid grid-cols-2 gap-6">
        <ZFormItem prop="name" :label="t('common.fields.name')" :show-message="false">
          <ZInput
            :model-value="form.name" :aria-label="t('common.fields.name')"
            :placeholder="t('keys.editor.namePlaceholder')"
            :disabled="saving"
            @update:model-value="updateForm('name', $event)"
          />
        </ZFormItem>
        <ZFormItem :label="t('keys.editor.labelOptional')">
          <ZInput
            :model-value="form.label" :aria-label="t('keys.editor.labelOptional')"
            :placeholder="t('keys.editor.forExampleBackendService')"
            :disabled="saving"
            @update:model-value="updateForm('label', $event)"
          />
        </ZFormItem>
      </div>

      <ZFormItem
        v-if="!editing"
        prop="customKey"
        :label="t('keys.editor.customKeyOptional')"
      >
        <ZInput
          :model-value="form.customKey" type="password"
          autocomplete="new-password"
          :spellcheck="false"
          :aria-label="t('keys.editor.customKeyOptional')"
          :placeholder="t('keys.editor.keyPlaceholder')"
          :disabled="saving"
          @update:model-value="updateForm('customKey', $event)"
        />
      </ZFormItem>

      <ZFormItem prop="groupIds" :label="t('groups.group')" :required="form.peerSharing.enabled">
        <AccountGroupCheckboxGrid
          :model-value="form.groupIds" :groups="groups"
          :loading="groupLoading"
          :disabled="saving"
          @update:model-value="updateForm('groupIds', $event)"
        />
      </ZFormItem>

      <ApiKeyPeerSharingField :sharing="form.peerSharing" :disabled="saving" @change="updatePeerSharing" />

      <ApiKeyLimitFields
        v-model:daily-limit-usd="form.dailyLimitUsd" v-model:weekly-limit-usd="form.weeklyLimitUsd"
        v-model:max-concurrency="form.maxConcurrency" v-model:requests-per-minute="form.requestsPerMinute"
        :disabled="saving"
      />

      <ProviderSettingsSection v-model="provider" :disabled="saving">
        <ZFormItem :label="t('keys.editor.upstreamIdentity')">
          <ProviderRequestProfilesEditor
            :model-value="form.providerRequestProfileOverrides" allow-inherit
            :active="open"
            :provider="provider"
            :disabled="saving"
            @update:model-value="updateForm('providerRequestProfileOverrides', $event)"
          />
        </ZFormItem>

        <InstallationSeedField
          v-if="provider === 'openai'"
          ref="seedField"
          v-model="form.installationSeed" :error="installationSeedError" :disabled="saving"
          :placeholder="t('settings.clientProfile.installationSeedPlaceholder')"
        />
      </ProviderSettingsSection>
    </ZForm>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        @click="requestSave"
      >
        {{ editing ? t('common.actions.save') : t('common.actions.create') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
