import type { Account, AccountImportTask } from '@/api'
import { computed, ref, shallowRef, watch } from 'vue'
import { createAccountImportTask, importAccounts } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage, generateRequestId } from '@/utils/operation'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'
import { accountCreateProvider, accountCreateSourceKey, accountImportModes, accountImportSettings, accountProxyError, emptyAccountCreateForm } from '../utils/accountCreate'
import { accountImportBatches, accountImportDocuments, mixedImportDocuments } from '../utils/accountImport'
import { apiKeyAccountError, emptyApiKeyAccountForm } from '../utils/upstreamApiKey'
import { useAccountAuthorization } from './useAccountAuthorization'

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
  onImportTaskCreated: (task: AccountImportTask, reveal?: boolean) => void
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<Account | null>(null)
  const creatingAccountAction = useAsyncAction()
  const createForm = ref(emptyAccountCreateForm())
  const authorization = useAccountAuthorization(() => finishCreate(reauthorizingAccount.value
    ? t('accounts.onboarding.reauthorized')
    : t('accounts.onboarding.accountAdded')))
  let submissionIds: string[] = []
  watch(createForm, () => {
    submissionIds = []
  }, { deep: true, flush: 'sync' })

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value)
        authorization.cancel()
    },
  })

  function clearCreate() {
    authorization.reset()
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
  }

  function requireImportProvider(provider: string | undefined) {
    if (!provider)
      throw new Error(t('accounts.onboarding.providerRequired'))
    if (!isSupportedProvider(provider))
      throw new Error(t('accounts.onboarding.accountPlatformIsNotSupported'))
    return provider
  }

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await authorization.complete()
      return
    }
    let submitted = 0
    await creatingAccountAction.run(async () => {
      const form = createForm.value
      const proxyError = accountProxyError(form)
      if (proxyError)
        throw new Error(proxyError)
      const settings = accountImportSettings(form)
      const outboundProxyId = form.proxyMode === 'proxy'
        ? form.proxyId.trim()
        : undefined
      const mode = form.mode
      if (mode === 'oauth')
        return
      const provider = accountCreateProvider(form)
      if (mode === 'api_key') {
        if (requireImportProvider(provider) !== 'openai')
          throw new Error(t('accounts.onboarding.apiKeyUnsupported'))
        const error = apiKeyAccountError(form.apiKey)
        if (error)
          throw new Error(error)
        await importAccounts({
          provider: 'openai',
          settings,
          outboundProxyId,
          data: {
            provider: 'openai',
            authentication_kind: 'api_key',
            name: form.apiKey.name.trim(),
            base_url: form.apiKey.base_url.trim(),
            api_key: form.apiKey.apiKey,
            transport: form.apiKey.transport,
          },
        })
        await finishCreate(t('accounts.onboarding.apiKeyAccountAdded'))
        return
      }
      const documents = form.source?.kind === 'bundle'
        ? mixedImportDocuments(form.importTexts.json)
        : accountImportDocuments(requireImportProvider(provider), mode, form.importTexts[mode])
      for (const document of documents)
        requireImportProvider(document.provider)
      const batches = accountImportBatches(documents.map(entry => ({
        provider: entry.provider,
        data: entry.document,
        settings,
        outboundProxyId,
      })))
      // 同一输入重试时复用每批标识，恢复丢失响应或部分提交，不重复执行 RT 交换
      const ids = submissionIds
      for (const [index, items] of batches.entries()) {
        const task = await createAccountImportTask({ submissionId: ids[index] ??= generateRequestId(), items })
        submitted += 1
        const finished = index === batches.length - 1
        if (finished)
          showCreateModal.value = false
        options.onImportTaskCreated(task, finished)
      }
      ZNotification.success({
        message: submitted > 1
          ? t('accounts.onboarding.importTasksCreated', { submitted })
          : t('accounts.onboarding.importTaskCreated'),
      })
    }, {
      onError: () => {
        if (submitted) {
          ZNotification.warning({
            message: t('accounts.onboarding.partialSubmission'),
          })
        }
      },
    })
  }

  async function handleAuthorizeOAuth() {
    if (authorization.busy.value)
      return
    try {
      const form = createForm.value
      const provider = accountCreateProvider(form)
      if (!isSupportedProvider(provider))
        throw new Error(t('accounts.onboarding.accountPlatformIsNotSupported'))
      const account = reauthorizingAccount.value
      const proxyError = accountProxyError(form)
      if (proxyError)
        throw new Error(proxyError)
      await authorization.start({
        start: {
          provider,
          name: account?.name || account?.email || t('accounts.onboarding.accountName', {
            provider: formatProviderLabel(provider),
          }),
          accountId: account?.id,
          outboundProxyId: account
            ? undefined
            : form.proxyMode === 'proxy'
              ? form.proxyId.trim()
              : undefined,
        },
        settings: account
          ? undefined
          : accountImportSettings(form),
      })
    }
    catch (cause) {
      ZNotification.error({
        message: errorMessage(cause, t('accounts.onboarding.failedToStartAuthorization')),
      })
    }
  }

  function openCreateAccount() {
    authorization.reset()
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: Account) {
    if (isSupportedProvider(account.provider) && account.authenticationKind !== 'oauth')
      return
    authorization.reset()
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyAccountCreateForm(),
      source: { kind: 'provider', id: account.provider },
      step: 'import',
    }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  async function finishCreate(message: string) {
    showCreateModal.value = false
    ZNotification.success({ message })
    await options.reload()
  }

  watch(() => accountCreateSourceKey(createForm.value), () => {
    createForm.value = {
      ...createForm.value,
      mode: reauthorizingAccount.value
        ? 'oauth'
        : createForm.value.source?.kind === 'bundle'
          ? 'json'
          : accountImportModes(accountCreateProvider(createForm.value))[0]?.value ?? 'json',
      apiKey: emptyApiKeyAccountForm(),
      importTexts: { access_token: '', refresh_token: '', json: '' },
    }
  }, { flush: 'sync' })

  watch([
    () => accountCreateSourceKey(createForm.value),
    () => createForm.value.mode,
    () => createForm.value.step,
    () => createForm.value.proxyMode,
    () => createForm.value.proxyMode === 'proxy'
      ? createForm.value.proxyId.trim()
      : '',
  ], () => {
    authorization.reset()
  }, { flush: 'sync' })

  return {
    showCreateModal,
    reauthorizingAccount,
    creatingAccount: creatingAccountAction.loading,
    authorizingOAuth: authorization.busy,
    authorization: authorization.view,
    authorizationCallback: authorization.callback,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
    clearCreate,
  }
}
