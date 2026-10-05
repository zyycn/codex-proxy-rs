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
    return { value: undefined, error: '请输入有效的 JSON 对象' }
  }
  if (!isRecord(value))
    return { value: undefined, error: '输入必须是 JSON 对象' }
  if (maximumBytes !== undefined && new TextEncoder().encode(JSON.stringify(value)).byteLength > maximumBytes)
    return { value: undefined, error: `输入不能超过 ${maximumBytes / 1024} KiB` }
  return { value, error: undefined }
}
