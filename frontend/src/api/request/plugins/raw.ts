import type { RequestContext, RequestNext } from './index'
import { t } from '@/i18n'
import { ApiError, normalizeApiResponseError } from './error'

export interface RawResponseMeta {
  status: number
  header: (name: string) => string | undefined
}

export interface RawResponse {
  status: number
  contentType: string
  body: ArrayBuffer
}

export interface RawResponseOptions {
  // 原始字节或宿主错误的正文上限；成功流由消费方约束，不在请求层缓冲
  maximumBytes: number
  accept: (response: RawResponseMeta) => boolean
}

export async function rawPlugin(context: RequestContext, next: RequestNext) {
  const options = context.raw
  if (!options)
    return next()
  context.config = {
    ...context.config,
    responseType: context.stream ? 'stream' : 'arraybuffer',
    validateStatus: () => true,
  }
  const response = await next()
  const meta: RawResponseMeta = {
    status: response.status,
    header: name => rawHeader(response.headers, name),
  }
  if (options.accept(meta)) {
    if (context.stream) {
      const headers = new Headers()
      for (const [name, value] of Object.entries(response.headers)) {
        if (value != null)
          headers.set(name, String(value))
      }
      return {
        ...response,
        data: new Response(response.data as ReadableStream<Uint8Array> | null, {
          status: response.status,
          statusText: response.statusText,
          headers,
        }),
      }
    }
    const body = response.data as ArrayBuffer
    if (body.byteLength > options.maximumBytes)
      throw new ApiError(t('common.errors.responseTooLarge'), 502, undefined, rawHeader(response.headers, 'x-request-id'), 'http')
    return {
      ...response,
      data: {
        status: response.status,
        contentType: rawHeader(response.headers, 'content-type') ?? 'application/octet-stream',
        body,
      } satisfies RawResponse,
    }
  }

  const body = context.stream
    ? await readErrorBody(response.data as ReadableStream<Uint8Array> | null, options.maximumBytes)
    : response.data as ArrayBuffer
  const decoded = decodeRawEnvelope(body)
  const normalized = normalizeApiResponseError({ ...response, data: decoded })
  if (normalized)
    throw normalized
  throw new ApiError(
    response.status >= 400
      ? t('common.errors.http', { status: response.status })
      : t('common.errors.unsupportedResponse'),
    response.status,
    undefined,
    rawHeader(response.headers, 'x-request-id'),
    'http',
  )
}

async function readErrorBody(body: ReadableStream<Uint8Array> | null, maximumBytes: number) {
  const reader = body?.getReader()
  if (!reader)
    return new ArrayBuffer(0)
  const chunks: Uint8Array[] = []
  let length = 0
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done)
        break
      length += value.byteLength
      if (length > maximumBytes)
        throw new ApiError(t('common.errors.responseTooLarge'), 502, undefined, undefined, 'http')
      chunks.push(value)
    }
  }
  finally {
    reader.releaseLock()
  }
  const bytes = new Uint8Array(length)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return bytes.buffer
}

function rawHeader(headers: unknown, name: string) {
  if (!headers || typeof headers !== 'object')
    return undefined
  const source = headers as { get?: (key: string) => unknown, [key: string]: unknown }
  const value = typeof source.get === 'function'
    ? source.get(name)
    : source[name]
  return typeof value === 'string' && value
    ? value
    : undefined
}

function decodeRawEnvelope(body: ArrayBuffer): unknown {
  if (body.byteLength === 0)
    return undefined
  try {
    return JSON.parse(new TextDecoder().decode(new Uint8Array(body))) as unknown
  }
  catch {
    return undefined
  }
}
