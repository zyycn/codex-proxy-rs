import type { Ref } from 'vue'
import type { AccountCapabilities, AccountPersonalInfoResponse } from '@/api'
import { computed, shallowRef, watch } from 'vue'

import { getAccountPersonalInfo } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

export function useAccountPersonalInfo({ accountId, open, capabilities }: {
  accountId: Ref<string>
  open: Ref<boolean>
  capabilities: Ref<AccountCapabilities>
}) {
  const info = shallowRef<AccountPersonalInfoResponse | null>(null)
  const request = useRequestState()
  const { loading } = request
  const profile = computed(() => capabilities.value.profile ? info.value?.profile ?? null : null)
  const subscription = computed(() => capabilities.value.subscription ? info.value?.subscription ?? null : null)
  const error = computed(() => request.error.value || (capabilities.value.profile ? info.value?.profileError : '') || '')

  async function load() {
    const targetAccountId = accountId.value
    if (!open.value || !targetAccountId || loading.value)
      return

    const version = request.start()
    try {
      const result = await getAccountPersonalInfo({ accountId: targetAccountId }, { signal: request.signal })
      if (!request.isCurrent(version))
        return
      // 单项失败仍更新其他信息；仅在当前打开周期保留上次可用的统计。
      info.value = { ...result, profile: result.profile ?? info.value?.profile ?? null }
    }
    catch (cause) {
      request.fail(version, cause)
    }
    finally {
      request.finish(version)
    }
  }

  // 打开或切换账号只请求一次；关闭取消等待，刷新按钮复用同一入口。
  watch([open, accountId, () => capabilities.value.profile, () => capabilities.value.subscription], ([isOpen]) => {
    request.invalidate({ resetLoading: isOpen })
    if (!isOpen)
      return
    info.value = null
    request.error.value = ''
    void load()
  }, { immediate: true })

  return {
    profile,
    subscription,
    loading,
    error,
    load,
  }
}
