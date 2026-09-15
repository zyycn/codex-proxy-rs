import type { OpsError, UsageRecordDetail } from '@/api'
import { opsErrorSummary } from './opsErrorPresentation'

export function requestDiagnosticsBundle(
  requestId: string,
  detail: UsageRecordDetail | null,
  errorRecord?: OpsError,
) {
  // 关联请求切换后，不能把原弹窗的错误归到另一个请求。
  const record = detail?.requestId === requestId ? detail : null
  const error = errorRecord?.requestId === requestId ? errorRecord : null
  const trace = record?.trace
  // 请求终态与错误事件可能来自不同尝试，不用事件的值填补终态空字段。
  const request = record ?? error

  return {
    schemaVersion: 2,
    exportedAt: new Date().toISOString(),
    source: 'usage.request_diagnostics',
    exportPolicy: 'allowlisted_fields_without_payloads',
    request: {
      requestId,
      source: record ? 'request_detail' : error ? 'error_record' : 'request_id_only',
      route: request?.route ?? null,
      provider: request?.provider ?? null,
      accountId: request?.accountId ?? null,
      clientTransport: request?.clientTransport ?? null,
      upstreamTransport: record ? record.upstreamTransport : error?.transport ?? null,
      upstreamRequestId: request?.upstreamRequestId ?? null,
      responseId: request?.responseId ?? null,
      statusCode: record?.statusCode ?? null,
      clientStatusCode: request?.clientStatusCode ?? null,
      upstreamStatusCode: request?.upstreamStatusCode ?? null,
      logicalOutcome: record?.logicalOutcome ?? null,
      attemptCount: record?.attemptCount ?? null,
      createdAt: request?.createdAt ?? null,
    },
    error: error
      ? {
          id: error.id,
          requestId: error.requestId,
          source: error.metadata.source,
          component: error.metadata.component,
          summary: opsErrorSummary(error),
          summarySource: 'failure_class_or_provider_error_code',
          failureClass: error.failureClass,
          providerErrorCode: error.providerErrorCode,
          upstreamSendState: error.upstreamSendState,
          attemptIndex: error.attemptIndex,
          clientStatusCode: error.clientStatusCode,
          upstreamStatusCode: error.upstreamStatusCode,
          upstreamRequestId: error.upstreamRequestId,
          responseId: error.responseId,
          latencyMs: error.latencyMs,
          createdAt: error.createdAt,
        }
      : null,
    // 旧 trace 的 sanitized 标记不是安全保证；不遍历或复制任何 event.data。
    trace: trace
      ? {
          schemaVersion: trace.schemaVersion,
          startedAtUnixMs: trace.startedAtUnixMs,
          totalEvents: trace.totalEvents,
          droppedEvents: trace.droppedEvents,
          events: trace.events.map(event => ({
            sequence: event.sequence,
            lastSequence: event.lastSequence,
            elapsedMs: event.elapsedMs,
            lastElapsedMs: event.lastElapsedMs,
            attemptIndex: event.attemptIndex,
            exchangeId: event.exchangeId,
            stage: event.stage,
            count: event.count,
          })),
        }
      : null,
    attempts: record?.attempts.map(attempt => ({
      id: attempt.id,
      attemptIndex: attempt.attemptIndex,
      trigger: attempt.trigger,
      provider: attempt.provider,
      transport: attempt.transport,
      sendState: attempt.sendState,
      outcome: attempt.outcome,
      downstreamCommitted: attempt.downstreamCommitted,
      statusCode: attempt.statusCode,
      providerErrorCode: attempt.providerErrorCode,
      failureClass: attempt.failureClass,
      accountId: attempt.accountId,
      firstTokenMs: attempt.firstTokenMs,
      latencyMs: attempt.latencyMs,
      startedAt: attempt.startedAt,
      completedAt: attempt.completedAt,
    })) ?? [],
    relatedRequests: record?.relatedRequests.map(related => ({
      requestId: related.requestId,
      relation: related.relation,
      outcome: related.outcome,
      completedAt: related.completedAt,
    })) ?? [],
    availability: {
      requestDetail: record ? 'available' : 'unavailable',
      errorSummary: error ? 'available' : 'not_available_for_selected_request',
      trace: !record ? 'unknown' : trace ? 'stages_only' : 'not_recorded',
      traceEventsDropped: trace?.droppedEvents ?? null,
      attemptsComplete: record?.attemptsComplete ?? null,
      relatedRequests: record ? 'available' : 'unknown',
      environment: 'not_collected',
    },
    omitted: [
      'error.message',
      'error.rawUpstreamError',
      'request.message',
      'request.metadata',
      'trace.events.data',
      'request_and_response_headers_and_bodies',
      'user_supplied_model_names',
      'account_names_emails_and_credential_names',
      'client_ip_user_agent_and_api_key',
    ],
    notes: [
      'schemaVersion 2 仅供人工排障；null 表示未知或未采集，不代表没有发生错误。',
      '错误摘要复用列表的分类展示，不是上游错误原文；error 为 null 时请结合 attempts 的分类与状态。',
      '时间线仅导出阶段、顺序和计时，不含事件 data；历史 sanitized 标记不作为安全依据。',
      'attemptsComplete 不为 true 时，尝试列表不完整；没有时间线的旧记录无法补回未采集事件。',
      '未自动采集网关版本、客户端版本、部署环境和故障发生时区，请另行补充。',
      '关联 ID 仍可能属于内部信息，分享前请审阅；原始错误、正文和日志需另行审阅脱敏，勿直接公开。',
    ],
  }
}
