<script setup lang="ts">
import type { ApiKeyLimitField } from '../composables/useApiKeyFormRules'
import type { AccountGroupRef, ApiKey, ApiKeyBatchChanges, ApiKeyPeerSharing, ProviderRequestProfileUpdates } from '@/api'
import { ZButton, ZConfirmDialog, ZDialog, ZForm, ZFormItem } from '@codex-proxy/ui'
import { KeyRound } from '@lucide/vue'
import { isEqual } from 'es-toolkit'
import { computed, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { batchUpdateApiKeys } from '@/api'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import ProviderRequestProfilesEditor from '@/components/request-profile/index.vue'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notification as ZNotification } from '@/plugins/notification'
import { PROVIDER_IDS } from '@/providers/catalog'
import { isValidInstallationSeed } from '@/providers/openai/request-profile/installationSeed'
import InstallationSeedField from '@/providers/openai/request-profile/InstallationSeedField.vue'
import { apiKeyFormValue } from '../composables/useApiKeyEditor'
import { apiKeyLimitFields, useApiKeyLimitRules } from '../composables/useApiKeyFormRules'
import ApiKeyLimitFields from './ApiKeyLimitFields.vue'
import ApiKeyPeerSharingField from './ApiKeyPeerSharingField.vue'

const props = defineProps<{
  keys: ApiKey[]
  groups: AccountGroupRef[]
  groupLoading: boolean
}>()
const emit = defineEmits<{ saved: [ids: string[]] }>()
const open = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const { loading, run } = useAsyncAction()
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const rules = useApiKeyLimitRules()
const confirmAll = ref(false)
function emptyForm() {
  return {
    groupIds: undefined as string[] | undefined,
    peerSharing: {} as Partial<ApiKeyPeerSharing>,
    profiles: {} as ProviderRequestProfileUpdates,
    installationSeed: '',
    dailyLimitUsd: '',
    weeklyLimitUsd: '',
    maxConcurrency: '',
    requestsPerMinute: '',
  }
}
const form = ref(emptyForm())
const originals = computed(() => props.keys.map(apiKeyFormValue))
const initialProfiles = computed(() => {
  const result: ProviderRequestProfileUpdates = {}
  for (const id of PROVIDER_IDS) {
    const first = originals.value[0]?.providerRequestProfileOverrides[id] ?? null
    if (originals.value.every(row => isEqual(first, row.providerRequestProfileOverrides[id] ?? null)))
      result[id] = first
  }
  return result
})
const hasExistingSeed = computed(() => originals.value.some(row => row.installationSeed))
const touchedLimits = ref(new Set<ApiKeyLimitField>())
function commonLimit(field: ApiKeyLimitField) {
  const first = originals.value[0]?.[field]
  return originals.value.every(row => row[field] === first)
    ? first
    : undefined
}
const mixedLimits = computed(() => apiKeyLimitFields.filter(field => commonLimit(field) === undefined && !touchedLimits.value.has(field)))
const initialSharing = computed(() => {
  const common: Partial<ApiKeyPeerSharing> = {}
  const first = props.keys[0]?.peerSharing
  if (first) {
    for (const field of [
      'enabled',
      'sharePool',
      'shareQuota',
      'sharePlans',
    ] as const) {
      if (props.keys.every(key => key.peerSharing[field] === first[field]))
        common[field] = first[field]
    }
  }
  return common
})
const sharingPatch = computed<Partial<ApiKeyPeerSharing>>(() => {
  const patch = form.value.peerSharing
  return patch.sharePool === false
    ? { ...patch, sharePlans: false }
    : patch
})
const sharing = computed(() => ({
  ...initialSharing.value,
  ...sharingPatch.value,
}))
function updateSharing(field: keyof ApiKeyPeerSharing, enabled: boolean) {
  const patch = form.value.peerSharing
  if (initialSharing.value[field] === enabled)
    delete patch[field]
  else
    patch[field] = enabled
  if (field === 'enabled' && !enabled) {
    form.value.peerSharing = patch.enabled === undefined
      ? {}
      : { enabled: false }
  }
}

function sameGroups(left: string[], right: string[]) {
  const ids = new Set(left)
  return ids.size === right.length && right.every(id => ids.has(id))
}
const initialGroups = computed(() => {
  const first = props.keys[0]?.groups.map(group => group.id) ?? []
  return props.keys.every(key => sameGroups(first, key.groups.map(group => group.id)))
    ? first
    : undefined
})
function updateGroups(ids: string[]) {
  form.value.groupIds = initialGroups.value && sameGroups(ids, initialGroups.value)
    ? undefined
    : ids
}

const seedError = computed(() => isValidInstallationSeed(form.value.installationSeed.trim())
  ? ''
  : t('settings.clientProfile.invalidInstallationSeed'))
const invalidScope = computed(() => props.keys.some(key =>
  (form.value.peerSharing.enabled ?? key.peerSharing.enabled)
  && (form.value.groupIds?.length ?? key.groups.length) === 0))
const changes = computed<ApiKeyBatchChanges>(() => {
  const draft = form.value
  const patch: ApiKeyBatchChanges = {}
  if (draft.groupIds !== undefined)
    patch.groupIds = draft.groupIds
  if (Object.keys(sharingPatch.value).length)
    patch.peerSharing = sharingPatch.value
  if (Object.keys(draft.profiles).length)
    patch.providerRequestProfileOverrides = draft.profiles
  if (draft.installationSeed.trim())
    patch.installationSeed = draft.installationSeed.trim()
  for (const field of touchedLimits.value) {
    const value = draft[field].trim()
    if (value === commonLimit(field))
      continue
    if (field === 'dailyLimitUsd' || field === 'weeklyLimitUsd')
      patch[field] = value || '0'
    else
      patch[field] = Number(value)
  }
  return patch
})
const canSave = computed(() => props.keys.length > 0 && Object.keys(changes.value).length > 0 && !invalidScope.value && !seedError.value)

watch(open, (visible) => {
  if (visible) {
    form.value = emptyForm()
    touchedLimits.value.clear()
    for (const field of apiKeyLimitFields)
      form.value[field] = commonLimit(field) ?? ''
  }
})

async function save() {
  if (!canSave.value || loading.value || !await formRef.value?.validate().catch(() => false))
    return
  if (form.value.groupIds?.length === 0) {
    confirmAll.value = true
    return
  }
  await applyChanges()
}
async function applyChanges() {
  confirmAll.value = false
  await run(async () => {
    const ids = props.keys.map(key => key.id)
    const result = await batchUpdateApiKeys({ ids, ...changes.value })
    open.value = false
    emit('saved', ids)
    ZNotification.success({
      message: t('keys.batch.updated', { count: result.updated }),
    })
  })
}
</script>

<template>
  <ZDialog
    v-model="open" :title="t('keys.batch.edit')" :description="t('keys.batch.description', { count: keys.length })" width="48rem" type="info"
    :show-close="!loading" :close-on-click-modal="!loading" :close-on-press-escape="!loading"
  >
    <template #icon>
      <KeyRound class="text-z-text" :size="20" aria-hidden="true" />
    </template>
    <ZForm ref="formElement" :model="form" :rules="rules" scroll-to-error focus-on-error novalidate class="grid gap-6" @submit="save">
      <ZFormItem :label="t('groups.group')" :error="invalidScope ? t('keys.editor.peerGroupRequired') : undefined">
        <AccountGroupCheckboxGrid
          :model-value="form.groupIds ?? initialGroups ?? []" :groups="groups" :loading="groupLoading" :disabled="loading"
          @update:model-value="updateGroups"
        />
        <p v-if="form.groupIds?.length === 0" class="mt-2 mb-0 text-z-sm text-z-text-secondary">
          {{ t('keys.scope.allAccounts.description') }}
        </p>
      </ZFormItem>

      <ApiKeyPeerSharingField :sharing="sharing" :disabled="loading" @change="updateSharing" />

      <div class="grid gap-4">
        <ZFormItem :label="t('keys.editor.upstreamIdentity')">
          <ProviderRequestProfilesEditor v-model:updates="form.profiles" :initial-profiles="initialProfiles" allow-inherit :active="open" :disabled="loading" />
        </ZFormItem>
        <InstallationSeedField v-model="form.installationSeed" :disabled="loading" :error="seedError" :placeholder="hasExistingSeed ? t('accounts.modelAccess.reserve') : t('settings.clientProfile.installationSeedPlaceholder')" />
      </div>
      <ApiKeyLimitFields
        v-model:daily-limit-usd="form.dailyLimitUsd" v-model:weekly-limit-usd="form.weeklyLimitUsd"
        v-model:max-concurrency="form.maxConcurrency" v-model:requests-per-minute="form.requestsPerMinute"
        :mixed="mixedLimits" :disabled="loading" @edit="touchedLimits.add($event)"
      />
    </ZForm>

    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :disabled="!canSave" :loading="loading" @click="save">
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
  <ZConfirmDialog
    v-model="confirmAll" :title="t('keys.scope.allAccounts.title')" :description="t('keys.scope.allAccounts.description')"
    :confirm-text="t('keys.scope.allAccounts.confirm')" :loading="loading" @confirm="applyChanges"
  />
</template>
