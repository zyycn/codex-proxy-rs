import { t } from '@/i18n'

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

type JsonObjectResult
  = | { value: Record<string, unknown>, error: undefined }
    | { value: undefined, error: string }

export function parseJsonObject(text: string, maximumBytes?: number): JsonObjectResult {
  let value: unknown
  try {
    value = JSON.parse(text)
  }
  catch {
    return {
      value: undefined,
      error: t('common.validation.invalidJson'),
    }
  }
  if (!isRecord(value)) {
    return {
      value: undefined,
      error: t('common.validation.jsonObject'),
    }
  }
  if (maximumBytes !== undefined && new TextEncoder().encode(JSON.stringify(value)).byteLength > maximumBytes) {
    return {
      value: undefined,
      error: t('common.validation.sizeLimit', { value: maximumBytes / 1024 }),
    }
  }
  return { value, error: undefined }
}
