import type { OpsError } from '@/api'

const failureClassLabels: Readonly<Record<string, string>> = {
  continuation_recovery_required: '会话续接需要重建',
}

export function failureClassText(value: string | null | undefined) {
  if (!value)
    return '未记录'
  return failureClassLabels[value] ?? value
}

export function opsErrorSummary(record: OpsError) {
  const failureLabel = failureClassLabels[record.failureClass]
  return failureLabel ?? record.providerErrorCode ?? record.failureClass
}
