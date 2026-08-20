import type { Account, AccountResetCredit } from '@/api'
import { computed, shallowRef, watch } from 'vue'

import {
  consumeAccountResetCredit,
  getAccountResetCredits,
  refreshAccountQuota,
} from '@/api'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

interface PendingResetCreditOperation {
  accountId: string
  creditId?: string
  redeemRequestId: string
  hasTransportFailure: boolean
}

interface ResetCreditsSnapshot {
  credits: AccountResetCredit[]
  availableCount: number
}

// 只保留本次前端会话中由用户主动查询得到的结果，避免账号列表渲染触发上游请求。
const snapshotsByAccountId = new Map<string, ResetCreditsSnapshot>()

export function useAccountResetCredits(options: {
  accountId: () => string
  onAccountUpdated: (account: Account) => void
}) {
  const credits = shallowRef<AccountResetCredit[]>([])
  const availableCount = shallowRef(0)
  const hasSnapshot = shallowRef(false)
  const loading = shallowRef(false)
  const consuming = shallowRef(false)
  const loadError = shallowRef('')
  const showConfirm = shallowRef(false)
  const pendingOperation = shallowRef<PendingResetCreditOperation | null>(null)
  let loadSequence = 0

  const availableCredits = computed(() =>
    credits.value.filter(credit => credit.status === 'available'),
  )
  const ambiguous = computed(() => pendingOperation.value?.hasTransportFailure === true)

  function applySnapshot(accountId: string, snapshot: ResetCreditsSnapshot) {
    snapshotsByAccountId.set(accountId, snapshot)
    if (accountId !== options.accountId())
      return
    credits.value = snapshot.credits
    availableCount.value = snapshot.availableCount
    hasSnapshot.value = true
  }

  function restoreSnapshot(accountId: string) {
    const snapshot = snapshotsByAccountId.get(accountId)
    credits.value = snapshot?.credits ?? []
    availableCount.value = snapshot?.availableCount ?? 0
    hasSnapshot.value = snapshot !== undefined
  }

  function applyConfirmedConsumption(operation: PendingResetCreditOperation) {
    const snapshot = snapshotsByAccountId.get(operation.accountId)
    if (!snapshot)
      return
    const creditIndex = operation.creditId
      ? snapshot.credits.findIndex(credit => credit.id === operation.creditId)
      : snapshot.credits.findIndex(credit => credit.status === 'available')
    const nextCredits = creditIndex < 0
      ? snapshot.credits
      : snapshot.credits.filter((_, index) => index !== creditIndex)
    applySnapshot(operation.accountId, {
      credits: nextCredits,
      availableCount: Math.max(0, snapshot.availableCount - 1),
    })
  }

  async function loadCredits() {
    const accountId = options.accountId()
    const sequence = ++loadSequence
    loading.value = true
    loadError.value = ''
    try {
      const result = await getAccountResetCredits({ accountId })
      if (sequence !== loadSequence || accountId !== options.accountId())
        return
      applySnapshot(accountId, {
        credits: result.credits,
        availableCount: Math.max(0, result.availableCount),
      })
    }
    catch (error: unknown) {
      if (sequence !== loadSequence || accountId !== options.accountId())
        return
      loadError.value = errorMessage(error, '重置卡查询失败')
    }
    finally {
      if (sequence === loadSequence)
        loading.value = false
    }
  }

  function requestConsume() {
    const accountId = options.accountId()
    const existing = pendingOperation.value
    if (!existing || existing.accountId !== accountId) {
      if (availableCount.value <= 0)
        return
      pendingOperation.value = {
        accountId,
        creditId: availableCredits.value[0]?.id,
        redeemRequestId: globalThis.crypto.randomUUID(),
        hasTransportFailure: false,
      }
    }
    showConfirm.value = true
  }

  function cancelConsume() {
    showConfirm.value = false
    if (!pendingOperation.value?.hasTransportFailure)
      pendingOperation.value = null
  }

  async function confirmConsume() {
    const operation = pendingOperation.value
    if (!operation || consuming.value)
      return

    consuming.value = true
    try {
      const result = await consumeAccountResetCredit({
        accountId: operation.accountId,
        creditId: operation.creditId,
        redeemRequestId: operation.redeemRequestId,
      })
      const confirmed = result.code === 'reset'
        || (result.code === 'already_redeemed' && operation.hasTransportFailure)
      pendingOperation.value = null
      showConfirm.value = false
      if (!confirmed) {
        toast.error(resetResultMessage(result.code))
        await loadCredits()
        return
      }

      toast.success(
        result.code === 'already_redeemed'
          ? '已确认上次操作完成'
          : '额度已重置',
      )
      applyConfirmedConsumption(operation)
      await loadCredits()
      try {
        const quota = await refreshAccountQuota({ accountId: operation.accountId })
        if (operation.accountId === options.accountId())
          options.onAccountUpdated(quota.account)
      }
      catch (error: unknown) {
        toast.warning(
          `重置卡已消费，额度快照刷新失败：${errorMessage(error, '请手动刷新额度')}`,
          { duration: 5000 },
        )
      }
    }
    catch (error: unknown) {
      showConfirm.value = false
      if (isAmbiguousConsumeError(error)) {
        pendingOperation.value = {
          ...operation,
          hasTransportFailure: true,
        }
        toast.warning('消费结果暂不确定；重试会复用同一个请求标识', { duration: 5000 })
      }
      else {
        pendingOperation.value = null
        toast.error(errorMessage(error, '额度重置失败'))
        await loadCredits()
      }
    }
    finally {
      consuming.value = false
    }
  }

  watch(
    options.accountId,
    (accountId) => {
      loadSequence += 1
      restoreSnapshot(accountId)
      loading.value = false
      loadError.value = ''
      pendingOperation.value = null
      showConfirm.value = false
    },
    { immediate: true },
  )

  return {
    credits,
    availableCredits,
    availableCount,
    hasSnapshot,
    loading,
    consuming,
    loadError,
    ambiguous,
    showConfirm,
    loadCredits,
    requestConsume,
    cancelConsume,
    confirmConsume,
  }
}

function isAmbiguousConsumeError(error: unknown) {
  if (!(error instanceof ApiError))
    return false
  if (error.status === 0 || error.status === 408 || error.message.includes('consume result is unknown'))
    return true
  return error.status >= 500
    && !error.message.startsWith('OpenAI reset-credit upstream returned HTTP ')
}

function resetResultMessage(code: string) {
  switch (code) {
    case 'already_redeemed':
      return '该重置操作已被处理，请先刷新重置卡列表'
    case 'no_credit':
      return '当前没有可用的主动重置卡'
    case 'nothing_to_reset':
      return '当前额度窗口不需要重置'
    default:
      return `上游未执行额度重置：${code}`
  }
}
