import type { ApiKey, ApiKeyPeerSharing } from '@/api'
import type { ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api/modules/settings/profiles'
import { cloneDeep } from 'es-toolkit'
import { ref, shallowRef } from 'vue'
import { createApiKey, updateApiKey } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'

export interface ApiKeyFormValue {
  peerSharing: ApiKeyPeerSharing
  providerRequestProfileOverrides: ProviderRequestProfiles
  installationId: string
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
    const profiles = cloneDeep(key.providerRequestProfileOverrides)
    const openai = profiles.openai
    const identity = (openai?.inherit as Record<string, unknown> | undefined) ?? openai
    const installationId = typeof identity?.installationId === 'string'
      ? identity.installationId
      : ''
    if (openai?.inherit)
      delete profiles.openai
    else if (openai)
      delete openai.installationId
    form.value = {
      peerSharing: { ...key.peerSharing },
      providerRequestProfileOverrides: profiles,
      installationId,
      customKey: '',
      name: key.name,
      label: key.label ?? '',
      groupIds: key.groups.map(group => group.id),
      maxConcurrency: limitInputValue(key.maxConcurrency),
      requestsPerMinute: limitInputValue(key.requestsPerMinute),
      dailyLimitUsd: limitInputValue(key.dailyLimitUsd),
      weeklyLimitUsd: limitInputValue(key.weeklyLimitUsd),
    }
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
        const profiles = cloneDeep(form.value.providerRequestProfileOverrides)
        const installationId = form.value.installationId
        if (installationId) {
          // 仅固定安装 ID 时保留对全局 UA 的继承，不在 Key 中复制当前全局值
          profiles.openai = profiles.openai
            ? { ...profiles.openai, installationId }
            : {
                inherit: { installationId },
              }
        }
        const current = editingKey.value
        let created: { key: string, name: string } | undefined
        if (current) {
          await updateApiKey({
            id: current.id,
            ...payload,
            providerRequestProfileOverrides: profileOverrideUpdates(
              current.providerRequestProfileOverrides,
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
          ZNotification.success({
            message: t('keys.editor.created'),
          })
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
    installationId: '',
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

function profileOverrideUpdates(
  previous: ProviderRequestProfiles,
  current: ProviderRequestProfiles,
): ProviderRequestProfileUpdates {
  const updates: ProviderRequestProfileUpdates = cloneDeep(current)
  for (const provider of Object.keys(previous)) {
    if (!Object.hasOwn(current, provider))
      updates[provider] = null
  }
  return updates
}

function limitInputValue(limit: string | number) {
  return Number(limit) === 0
    ? ''
    : String(limit)
}
