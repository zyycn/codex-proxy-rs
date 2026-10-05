import type { Ref } from 'vue'
import type { PluginRefreshContext } from '../utils/actions'
import type { CreatePluginSourceCredentialRequest, PluginSourceCredential } from '@/api'
import { toast } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { createPluginSourceCredential, deletePluginSourceCredential } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notifyPluginError } from '../utils/actions'

interface CredentialContext extends PluginRefreshContext {
  credentials: Ref<PluginSourceCredential[]>
}

export function usePluginCredentials({ credentials, refresh }: CredentialContext) {
  const action = useAsyncAction({
    errorText: false,
    onError: error => notifyPluginError('下载认证保存失败', error),
  })
  const savingCredential = action.loading
  const showCredentialDelete = shallowRef(false)
  const pendingCredential = shallowRef<PluginSourceCredential | null>(null)
  const busyCredentialId = shallowRef('')

  async function saveCredential(request: CreatePluginSourceCredentialRequest) {
    const result = await action.run(() => createPluginSourceCredential(request, { silent: true }))
    if (!result)
      return null
    credentials.value = [...credentials.value, result]
    return result
  }

  function requestCredentialDelete(credential: PluginSourceCredential) {
    pendingCredential.value = credential
    showCredentialDelete.value = true
  }

  async function confirmCredentialDelete() {
    const credential = pendingCredential.value
    if (!credential || busyCredentialId.value)
      return
    busyCredentialId.value = credential.id
    try {
      await deletePluginSourceCredential({ id: credential.id }, { silent: true })
      showCredentialDelete.value = false
      toast.success('来源凭据已删除')
      await refresh(true)
    }
    catch (error) {
      notifyPluginError('来源凭据删除失败', error)
    }
    finally {
      busyCredentialId.value = ''
    }
  }

  return { savingCredential, showCredentialDelete, pendingCredential, busyCredentialId, saveCredential, requestCredentialDelete, confirmCredentialDelete }
}
