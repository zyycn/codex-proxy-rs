import type { ApiKey, ApiKeyPeerSharing, ProviderRequestProfiles } from '@/api'
import { cloneDeep } from 'es-toolkit'
import { ref, shallowRef } from 'vue'
import { createApiKey, updateApiKey } from '@/api'
import { requestProfileChanges } from '@/components/request-profile/changes'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/plugins/notification'
import { decodeKeyProfiles, encodeKeyProfiles } from '@/providers/openai/request-profile/installationSeed'

export interface ApiKeyFormValue {
  peerSharing: ApiKeyPeerSharing
  providerRequestProfileOverrides: ProviderRequestProfiles
  installationSeed: string
  customKey: string
  name: string
  label: string
  groupIds: string[]
  maxConcurrency: string
  requestsPerMinute: string
  dailyLimitUsd: string
  weeklyLimitUsd: string
}

export function useApiKeyEditor(options: {
  reload: () => Promise<unknown>
  onCreated: (key: string, name: string) => void
}) {
  const showFormModal = shallowRef(false)
  const showAllAccountsConfirm = shallowRef(false)
  const editingKey = shallowRef<ApiKey | null>(null)
  const savingKeyAction = useAsyncAction()
  const savingKey = savingKeyAction.loading
  const form = ref<ApiKeyFormValue>(emptyForm())

  function openCreate() {
    editingKey.value = null
    form.value = emptyForm()
    showFormModal.value = true
  }

  function openEdit(key: ApiKey) {
    editingKey.value = key
    form.value = apiKeyFormValue(key)
    showFormModal.value = true
  }

  function requestSave() {
    if (savingKey.value)
      return
    if (form.value.groupIds.length === 0 && !form.value.peerSharing.enabled) {
      showAllAccountsConfirm.value = true
      return
    }
    void save()
  }

  async function confirmAllAccountsScope() {
    showAllAccountsConfirm.value = false
    await save()
  }

  async function save() {
    if (savingKey.value)
      return

    await savingKeyAction.run(
      async () => {
        const payload = {
          peerSharing: { ...form.value.peerSharing },
          name: form.value.name.trim(),
          label: form.value.label.trim() || null,
          groupIds: [
            ...new Set(form.value.groupIds),
          ],
          maxConcurrency: Number(form.value.maxConcurrency),
          requestsPerMinute: Number(form.value.requestsPerMinute),
          dailyLimitUsd: form.value.dailyLimitUsd.trim() || '0',
          weeklyLimitUsd: form.value.weeklyLimitUsd.trim() || '0',
        }
        const profiles = encodeKeyProfiles(form.value.providerRequestProfileOverrides, form.value.installationSeed)
        const current = editingKey.value
        let created: { key: string, name: string } | undefined
        if (current) {
          const original = decodeKeyProfiles(current.providerRequestProfileOverrides)
          await updateApiKey({
            id: current.id,
            ...payload,
            providerRequestProfileOverrides: requestProfileChanges(
              encodeKeyProfiles(original.profiles, original.installationSeed),
              profiles,
            ),
          })
        }
        else {
          const result = await createApiKey({
            ...payload,
            providerRequestProfileOverrides: profiles,
            customKey: form.value.customKey || undefined,
          })
          created = { key: result.plaintextKey, name: payload.name }
        }

        showFormModal.value = false
        await options.reload()
        if (current) {
          ZNotification.success({
            message: t('keys.editor.updated'),
          })
        }
        else if (created) {
          options.onCreated(created.key, created.name)
        }
      },
      {
        onError: () => void options.reload(),
      },
    )
  }

  function clearCustomKey() {
    form.value.customKey = ''
  }

  return {
    showFormModal,
    showAllAccountsConfirm,
    editingKey,
    savingKey,
    form,
    openCreate,
    openEdit,
    clearCustomKey,
    requestSave,
    confirmAllAccountsScope,
  }
}

function emptyForm(): ApiKeyFormValue {
  return {
    peerSharing: {
      enabled: false,
      sharePool: false,
      shareQuota: false,
      sharePlans: false,
    },
    providerRequestProfileOverrides: {},
    installationSeed: '',
    customKey: '',
    name: '',
    label: '',
    groupIds: [],
    maxConcurrency: '',
    requestsPerMinute: '',
    dailyLimitUsd: '',
    weeklyLimitUsd: '',
  }
}

function limitInputValue(limit: string | number) {
  return Number(limit) === 0
    ? ''
    : String(limit)
}

export function apiKeyFormValue(key: ApiKey): ApiKeyFormValue {
  const { profiles, installationSeed } = decodeKeyProfiles(cloneDeep(key.providerRequestProfileOverrides))
  return {
    peerSharing: { ...key.peerSharing },
    providerRequestProfileOverrides: profiles,
    installationSeed,
    customKey: '',
    name: key.name,
    label: key.label ?? '',
    groupIds: key.groups.map(group => group.id),
    maxConcurrency: limitInputValue(key.maxConcurrency),
    requestsPerMinute: limitInputValue(key.requestsPerMinute),
    dailyLimitUsd: limitInputValue(key.dailyLimitUsd),
    weeklyLimitUsd: limitInputValue(key.weeklyLimitUsd),
  }
}
