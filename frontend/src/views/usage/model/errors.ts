import type { ClientOpsError, OpsError } from '@/api'

const failureClassLabels: Readonly<Record<string, string>> = {
  continuation_recovery_required: '会话续接需要重建',
}

export function failureClassText(value: string | null | undefined) {
  if (!value)
    return '未记录'
  return failureClassLabels[value] ?? value
}

export function opsErrorSummary(record: OpsError) {
  return failureClassLabels[record.failureClass]
    ?? record.providerErrorCode
    ?? record.failureClass
}

export interface UsageErrorRecord {
  id: string
  requestId: string | null
  provider?: string | null
  authenticationKind?: string | null
  accountId?: string | null
  accountName?: string | null
  accountEmail?: string | null
  summary: string
  message: string
  recoveredAt: string | null
  upstreamSendState: string | null
  model: string | null
  requestedModel: string | null
  upstreamModel: string | null
  route: string
  createdAtDisplay: string
  clientIp: string | null
  userAgent: string | null
  adminRecord?: OpsError
}

export function adminUsageError(record: OpsError): UsageErrorRecord {
  return {
    id: record.id,
    requestId: record.requestId,
    provider: record.provider,
    authenticationKind: record.authenticationKind,
    accountId: record.accountId,
    accountName: record.accountName,
    accountEmail: record.accountEmail,
    summary: opsErrorSummary(record),
    message: record.message,
    recoveredAt: record.metadata.recoveredAt,
    upstreamSendState: record.upstreamSendState,
    model: record.model,
    requestedModel: record.requestedModel,
    upstreamModel: record.upstreamModel,
    route: record.route,
    createdAtDisplay: record.createdAtDisplay,
    clientIp: record.clientIp,
    userAgent: record.userAgent,
    adminRecord: record,
  }
}

export function keyUsageError(record: ClientOpsError): UsageErrorRecord {
  return {
    id: record.id,
    requestId: record.requestId,
    summary: failureClassLabels[record.failureClass] ?? record.failureClass,
    message: '',
    recoveredAt: record.recoveredAt,
    upstreamSendState: record.upstreamSendState,
    model: record.model,
    requestedModel: record.requestedModel,
    upstreamModel: record.upstreamModel,
    route: record.route,
    createdAtDisplay: record.createdAtDisplay,
    clientIp: record.clientIp,
    userAgent: record.userAgent,
  }
}
