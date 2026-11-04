<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyEditor'

import type { AccountGroupRef, ApiKeyPeerSharing } from '@/api'
import { ZButton, ZCheckbox, ZDialog, ZForm, ZFormItem, ZIconButton, ZInput, ZSwitch } from '@codex-proxy/ui'
import { Dices, DollarSign, KeyRound } from '@lucide/vue'

import { computed, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { isValidInstallationId } from '@/api/modules/settings/profiles'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'
import HelpPopover from '@/components/HelpPopover.vue'
import { generateRequestId as generateUuid } from '@/utils/operation'
import { useApiKeyFormRules } from '../composables/useApiKeyFormRules'

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
const title = computed(() => props.editing
  ? t('keys.actions.editKey')
  : t('keys.filters.createApiKey'))
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const rules = useApiKeyFormRules(() => form.value)

const installationIdError = computed(() => isValidInstallationId(form.value.installationId)
  ? ''
  : t('settings.clientProfile.invalidInstallationId'))

watch(open, () => formRef.value?.clearValidate())

async function requestSave() {
  if (props.saving || installationIdError.value || !await formRef.value?.validate().catch(() => false))
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

      <ZFormItem :label="t('keys.editor.peerSharing')">
        <template #label-extra>
          <HelpPopover :label="t('keys.editor.peerSharing')" :content="t('keys.editor.peerSharingHelp')" />
        </template>
        <div class="flex flex-wrap items-center gap-x-5 gap-y-3">
          <ZSwitch
            :model-value="form.peerSharing.enabled"
            :aria-label="t('keys.editor.peerSharing')"
            :disabled="saving"
            @update:model-value="updatePeerSharing('enabled', $event)"
          />
          <template v-if="form.peerSharing.enabled">
            <ZCheckbox :model-value="form.peerSharing.sharePool" :label="t('keys.editor.sharePool')" :disabled="saving" @update:model-value="updatePeerSharing('sharePool', $event)" />
            <ZCheckbox :model-value="form.peerSharing.shareQuota" :label="t('keys.editor.shareQuota')" :disabled="saving" @update:model-value="updatePeerSharing('shareQuota', $event)" />
            <ZCheckbox :model-value="form.peerSharing.sharePlans" :label="t('keys.editor.sharePlans')" :disabled="saving || !form.peerSharing.sharePool" @update:model-value="updatePeerSharing('sharePlans', $event)" />
          </template>
        </div>
      </ZFormItem>

      <div class="grid gap-4">
        <ZFormItem :label="t('keys.editor.upstreamIdentity')">
          <ProviderRequestProfilesEditor
            :model-value="form.providerRequestProfileOverrides" allow-inherit
            :active="open"
            :disabled="saving"
            @update:model-value="updateForm('providerRequestProfileOverrides', $event)"
          />
        </ZFormItem>

        <ZFormItem :label="t('settings.clientProfile.installationId')" :error="installationIdError">
          <template #label-extra>
            <span class="-my-2 inline-flex">
              <HelpPopover :label="t('settings.clientProfile.installationId')" :content="t('settings.clientProfile.installationIdHelp')" />
            </span>
          </template>
          <template #extra>
            <ZIconButton
              variant="link" size="small" class="-my-2"
              :aria-label="t('settings.clientProfile.generateInstallationId')"
              :disabled="saving"
              @click="updateForm('installationId', generateUuid())"
            >
              <Dices class="size-4" aria-hidden="true" />
            </ZIconButton>
          </template>
          <ZInput
            :model-value="form.installationId"
            :aria-label="t('settings.clientProfile.installationId')"
            :aria-invalid="!!installationIdError"
            :placeholder="t('settings.clientProfile.installationIdPlaceholder')"
            :disabled="saving"
            :spellcheck="false"
            autocomplete="off"
            class="font-mono"
            @update:model-value="updateForm('installationId', $event)"
          />
        </ZFormItem>
      </div>

      <div class="grid gap-6 sm:grid-cols-2">
        <ZFormItem prop="dailyLimitUsd" :label="t('keys.editor.dailyLimit')">
          <ZInput
            :model-value="form.dailyLimitUsd" type="number"
            min="0"
            step="any"
            :aria-label="t('keys.editor.dailyLimitUsd')"
            :placeholder="t('common.values.unlimited')"
            :disabled="saving"
            @update:model-value="updateForm('dailyLimitUsd', $event)"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </ZInput>
        </ZFormItem>
        <ZFormItem prop="weeklyLimitUsd" :label="t('keys.editor.weeklyLimit')">
          <ZInput
            :model-value="form.weeklyLimitUsd" type="number"
            min="0"
            step="any"
            :aria-label="t('keys.editor.weeklyLimitUsd')"
            :placeholder="t('common.values.unlimited')"
            :disabled="saving"
            @update:model-value="updateForm('weeklyLimitUsd', $event)"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </ZInput>
        </ZFormItem>
      </div>

      <div class="grid gap-6 sm:grid-cols-2">
        <ZFormItem prop="maxConcurrency" :label="t('keys.editor.maximumConcurrency')">
          <ZInput
            :model-value="form.maxConcurrency" type="number"
            :aria-label="t('keys.editor.maximumConcurrency')"
            min="0"
            step="1"
            :placeholder="t('common.values.unlimited')"
            :disabled="saving"
            @update:model-value="updateForm('maxConcurrency', $event)"
          />
        </ZFormItem>
        <ZFormItem prop="requestsPerMinute" :label="t('keys.editor.requestsPerMinuteRpm')">
          <ZInput
            :model-value="form.requestsPerMinute" type="number"
            :aria-label="t('keys.editor.requestsPerMinute')"
            min="0"
            step="1"
            :placeholder="t('common.values.unlimited')"
            :disabled="saving"
            @update:model-value="updateForm('requestsPerMinute', $event)"
          />
        </ZFormItem>
      </div>
    </ZForm>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="!!installationIdError"
        @click="requestSave"
      >
        {{ editing ? t('common.actions.save') : t('common.actions.create') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
