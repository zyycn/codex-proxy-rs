import type { UsageRecordDetail } from '@/api'
import { t } from '@/i18n'
import { isRecord } from '@/utils/data'

// 详情弹窗共享的字段展示约定：占位符、标签/值样式，供弹窗与其子组件复用。
export const fieldLabelClass = 'text-cp-xs leading-none font-bold text-cp-text-quaternary'
export const fieldValueBaseClass
  = 'mt-1.5 mb-0 min-w-0 truncate text-cp-sm leading-none font-bold text-cp-text'

export function displayValue(value: unknown) {
  if (value === undefined || value === null || value === '')
    return '—'
  return String(value)
}

export function fieldValueClass(mono?: boolean) {
  return [
    fieldValueBaseClass,
    mono ? 'font-mono tabular-nums' : undefined,
  ]
}

export function isErrorUsageRecord(record: Pick<UsageRecordDetail, 'logicalOutcome'> | null) {
  return record?.logicalOutcome === 'failed' || record?.logicalOutcome === 'incomplete'
}

export function attemptOutcomeText(outcome: string) {
  const labels: Record<string, string> = {
    succeeded: t('common.status.success'),
    failed: t('common.status.failed'),
    cancelled: t('usage.status.cancelled'),
    incomplete: t('usage.health.notCompleted'),
    running: t('usage.request.inProgress'),
  }
  return labels[outcome] ?? outcome
}

export function attemptOutcomeClass(outcome: string) {
  if (outcome === 'succeeded')
    return 'text-cp-success-text'
  if (outcome === 'failed')
    return 'text-cp-error-text'
  return 'text-cp-text-secondary'
}

export function visibleRequestText(record: UsageRecordDetail) {
  const body = record.metadata.requestBody
  if (!body)
    return ''

  return extractInputText(body) || JSON.stringify(body, null, 2)
}

export function visibleResponseText(record: UsageRecordDetail) {
  const body = record.metadata.responseBody
  if (!body)
    return ''

  if (typeof body === 'string')
    return body

  return stringProperty(asRecord(body), 'output_text') || extractOutputText(body) || JSON.stringify(body, null, 2)
}

function extractInputText(body: unknown) {
  const input = asRecord(body)?.input
  if (typeof input === 'string')
    return input

  if (!Array.isArray(input))
    return ''

  return input
    .flatMap((item) => {
      const content = asRecord(item)?.content
      if (typeof content === 'string')
        return [content]

      if (!Array.isArray(content))
        return []

      return content.flatMap((part) => {
        const value = asRecord(part)
        const text = stringProperty(value, 'text')
        return value?.type === 'input_text' && text
          ? [text]
          : []
      })
    })
    .filter(Boolean)
    .join('\n')
}

function extractOutputText(body: unknown) {
  const output = asRecord(body)?.output
  if (!Array.isArray(output))
    return ''

  return output
    .flatMap((item) => {
      const content = asRecord(item)?.content
      if (!Array.isArray(content))
        return []
      return content.flatMap((part) => {
        const text = stringProperty(asRecord(part), 'text')
        return text
          ? [text]
          : []
      })
    })
    .filter(Boolean)
    .join('\n')
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return isRecord(value)
    ? value
    : undefined
}

function stringProperty(value: Record<string, unknown> | undefined, key: string) {
  const valueAtKey = value?.[key]
  return typeof valueAtKey === 'string'
    ? valueAtKey
    : undefined
}
