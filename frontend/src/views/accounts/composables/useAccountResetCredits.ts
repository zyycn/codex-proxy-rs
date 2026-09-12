import type { Account, AccountResetCredit } from '@/api'
import { computed, shallowReactive, shallowRef, watch } from 'vue'

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
  creditId: string
  credit: AccountResetCredit
  redeemRequestId: string
  hasTransportFailure: boolean
}

interface ResetCreditsSnapshot {
  credits: AccountResetCredit[]
  availableCount: number
}

interface ResetCreditsSession {
  accountId: string
  snapshot: ResetCreditsSnapshot | null
  pendingOperation: PendingResetCreditOperation | null
  consuming: boolean
  loading: boolean
  loadError: string
  loadSequence: number
  accountUpdatedListeners: Set<(account: Account) => void>
}

// 库存仍以主动查询的上游结果为准；未决操作和消费锁必须跨展开行卸载存续。
const sessionsByAccountId = new Map<string, ResetCreditsSession>()

function getResetCreditsSession(accountId: string) {
  let session = sessionsByAccountId.get(accountId)
  if (!session) {
    session = shallowReactive<ResetCreditsSession>({
      accountId,
      snapshot: null,
      pendingOperation: null,
      consuming: false,
      loading: false,
      loadError: '',
      loadSequence: 0,
      accountUpdatedListeners: new Set(),
    })
    sessionsByAccountId.set(accountId, session)
  }
  return session
}

async function loadSessionCredits(session: ResetCreditsSession) {
  const sequence = ++session.loadSequence
  session.loading = true
  session.loadError = ''
  try {
    const result = await getAccountResetCredits({ accountId: session.accountId })
    if (sequence !== session.loadSequence)
      return
    session.snapshot = {
      credits: result.credits,
      availableCount: Math.max(0, result.availableCount),
    }
  }
  catch (error: unknown) {
    if (sequence === session.loadSequence)
      session.loadError = errorMessage(error, '重置卡查询失败')
  }
  finally {
    if (sequence === session.loadSequence)
      session.loading = false
  }
}

export function useAccountResetCredits(options: {
  accountId: () => string
  onAccountUpdated: (account: Account) => void
}) {
  const session = shallowRef(getResetCreditsSession(options.accountId()))
  const credits = computed(() => session.value.snapshot?.credits ?? [])
  const availableCount = computed(() => session.value.snapshot?.availableCount ?? 0)
  const hasSnapshot = computed(() => session.value.snapshot !== null)
  const loading = computed(() => session.value.loading)
  const consuming = computed(() => session.value.consuming)
  const loadError = computed(() => session.value.loadError)
  const showConfirm = shallowRef(false)
  const selectedCreditId = shallowRef('')
  const pendingOperation = computed(() => session.value.pendingOperation)

  const availableCredits = computed(() =>
    credits.value.filter(credit => credit.status === 'available'),
  )
  const selectedCredit = computed(() =>
    availableCredits.value.find(credit => credit.id === selectedCreditId.value),
  )
  const consumptionCredit = computed(() =>
    pendingOperation.value?.credit ?? selectedCredit.value,
  )
  const ambiguous = computed(() => pendingOperation.value?.hasTransportFailure === true)
  const canRequestConsume = computed(() =>
    ambiguous.value || (availableCount.value > 0 && selectedCredit.value !== undefined),
  )

  function reconcileSelectedCredit() {
    const operation = pendingOperation.value
    if (operation) {
      selectedCreditId.value = operation.creditId
      return
    }

    if (!selectedCredit.value)
      selectedCreditId.value = ''
  }

  function selectCredit(creditId: string) {
    if (pendingOperation.value || consuming.value)
      return
    if (!availableCredits.value.some(credit => credit.id === creditId))
      return
    selectedCreditId.value = creditId
  }

  function applyConfirmedConsumption(target: ResetCreditsSession, operation: PendingResetCreditOperation) {
    const snapshot = target.snapshot
    if (!snapshot)
      return
    const creditIndex = snapshot.credits.findIndex(credit => credit.id === operation.creditId)
    const nextCredits = creditIndex < 0
      ? snapshot.credits
      : snapshot.credits.filter((_, index) => index !== creditIndex)
    target.snapshot = {
      credits: nextCredits,
      availableCount: Math.max(0, snapshot.availableCount - 1),
    }
  }

  function requestConsume() {
    if (consuming.value || loading.value || !canRequestConsume.value)
      return
    showConfirm.value = true
  }

  function cancelConsume() {
    if (consuming.value)
      return
    showConfirm.value = false
  }

  async function confirmConsume(): Promise<boolean> {
    const target = session.value
    if (!showConfirm.value || target.consuming || target.loading)
      return false

    const credit = selectedCredit.value
    // 确认发送时才建立操作；仅打开或取消确认弹窗不占用账号的消费状态。
    const operation = target.pendingOperation ?? (credit && availableCount.value > 0
      ? {
          accountId: target.accountId,
          creditId: credit.id,
          credit,
          redeemRequestId: generateRedeemRequestId(),
          hasTransportFailure: false,
        }
      : null)
    if (!operation)
      return false
    target.pendingOperation = operation
    target.consuming = true
    try {
      const result = await consumeAccountResetCredit({
        accountId: operation.accountId,
        creditId: operation.creditId,
        redeemRequestId: operation.redeemRequestId,
      })
      const confirmed = result.code === 'reset'
        || (result.code === 'already_redeemed' && operation.hasTransportFailure)
      target.pendingOperation = null
      if (!confirmed) {
        toast.error(resetResultMessage(result.code))
        await loadSessionCredits(target)
        return false
      }

      const successMessage = result.code === 'already_redeemed'
        ? '上次重置已完成'
        : '额度已重置'
      applyConfirmedConsumption(target, operation)
      await loadSessionCredits(target)
      try {
        const quota = await refreshAccountQuota({ accountId: operation.accountId })
        // 原组件可能已经卸载或换号，只通知仍订阅该账号的实例。
        for (const listener of target.accountUpdatedListeners)
          listener(quota.account)
        toast.success(successMessage)
      }
      catch (error: unknown) {
        toast.warning(
          `${successMessage}，但最新额度加载失败：${errorMessage(error, '请手动刷新额度')}`,
          { duration: 5000 },
        )
      }
      return true
    }
    catch (error: unknown) {
      if (isAmbiguousConsumeError(error)) {
        target.pendingOperation = {
          ...operation,
          hasTransportFailure: true,
        }
        toast.warning('消费结果暂不确定；重试会复用同一个请求标识', { duration: 5000 })
      }
      else {
        target.pendingOperation = null
        toast.error(errorMessage(error, '额度重置失败'))
        await loadSessionCredits(target)
      }
      return false
    }
    finally {
      target.consuming = false
    }
  }

  watch(
    options.accountId,
    (accountId, _, onCleanup) => {
      const target = getResetCreditsSession(accountId)
      session.value = target
      selectedCreditId.value = ''
      showConfirm.value = false
      const listener = (account: Account) => options.onAccountUpdated(account)
      target.accountUpdatedListeners.add(listener)
      onCleanup(() => target.accountUpdatedListeners.delete(listener))
    },
    { immediate: true, flush: 'sync' },
  )

  watch([credits, pendingOperation], reconcileSelectedCredit, { immediate: true, flush: 'sync' })
  watch(consuming, (isConsuming) => {
    if (!isConsuming)
      showConfirm.value = false
  }, { flush: 'sync' })

  return {
    credits,
    availableCredits,
    availableCount,
    selectedCreditId,
    consumptionCredit,
    canRequestConsume,
    hasSnapshot,
    loading,
    consuming,
    loadError,
    ambiguous,
    showConfirm,
    loadCredits: () => loadSessionCredits(session.value),
    selectCredit,
    requestConsume,
    cancelConsume,
    confirmConsume,
  }
}

function generateRedeemRequestId() {
  // 普通 HTTP 管理端没有 randomUUID；getRandomValues 仍可生成密码学安全的 UUIDv4。
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0F) | 0x40
  bytes[8] = (bytes[8] & 0x3F) | 0x80
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

function isAmbiguousConsumeError(error: unknown) {
  if (!(error instanceof ApiError))
    return false
  return error.code === 50202
    || error.status === 0
    || error.status === 408
    || error.kind === 'timeout'
    || error.kind === 'network'
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
