import type { PluginBridgeMessage, PluginModelMessageType } from './bridge-contract'
import type { PluginManagementView } from '@/api'
import { callPluginModelResponses } from '@/api'
import { t } from '@/i18n'
import { errorMessage } from '@/utils/operation'
import { hasControlCharacter, isRecord, MAXIMUM_MODEL_BODY_BYTES, MAXIMUM_MODEL_CHUNK_BYTES, MAXIMUM_MODEL_IN_FLIGHT, MODEL_REQUEST_DEADLINE_MS, MODEL_RESPONSE_IDLE_MS } from './bridge-contract'

interface ModelRequest {
  id: number
  source: Window
  controller: AbortController
  reader?: ReadableStreamDefaultReader<Uint8Array>
  buffered?: Uint8Array
  bufferedOffset: number
  reading: boolean
  nextPullId: number
  activePullId?: number
  totalTimer?: ReturnType<typeof setTimeout>
  idleTimer?: ReturnType<typeof setTimeout>
}

// 一个页面会话拥有全部模型流，页面销毁后迟到的响应只会被释放
export function createPluginModelStreams(options: {
  target: PluginManagementView['target']
  requestIds: ReadonlySet<number>
  post: (source: Window, type: PluginModelMessageType, id: number, payload?: unknown, error?: string, transfer?: Transferable[]) => void
}) {
  const requests = new Map<number, ModelRequest>()
  function validateModelRequestPayload(payload: unknown) {
    if (!isRecord(payload))
      throw new Error(t('plugins.frame.invalidModelRequest'))
    const clientKeyId = payload.clientKeyId
    const body = payload.body
    if (
      typeof clientKeyId !== 'string'
      || !clientKeyId
      || clientKeyId !== clientKeyId.trim()
      || clientKeyId.length > 128
      || hasControlCharacter(clientKeyId)
    ) {
      throw new Error(t('plugins.frame.clientkeyidIsInvalid'))
    }
    if (typeof body !== 'string' || new TextEncoder().encode(body).byteLength > MAXIMUM_MODEL_BODY_BYTES)
      throw new Error(t('plugins.frame.invalidModelBody'))
    let parsed: unknown
    try {
      parsed = JSON.parse(body)
    }
    catch {
      throw new Error(t('plugins.frame.invalidModelJson'))
    }
    if (!isRecord(parsed))
      throw new Error(t('plugins.frame.modelObjectRequired'))
    return { clientKeyId, body }
  }

  function supportedModelResponse(response: Response) {
    if (!response.body)
      return true
    const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase()
    return contentType === 'application/json'
      || contentType === 'text/event-stream'
      || Boolean(contentType?.endsWith('+json'))
  }

  function detachModelRequest(request: ModelRequest) {
    if (requests.get(request.id) !== request)
      return false
    requests.delete(request.id)
    clearTimeout(request.totalTimer)
    clearTimeout(request.idleTimer)
    request.totalTimer = undefined
    request.idleTimer = undefined
    return true
  }

  function cancelModelReader(request: ModelRequest) {
    const reader = request.reader
    if (!reader)
      return
    void reader.cancel()
      .finally(() => {
        try {
          reader.releaseLock()
        }
        catch {
          // 取消中的 read 会自行释放；会话清理不能因此产生未处理拒绝。
        }
      })
      .catch(() => undefined)
  }

  function terminateModelRequest(request: ModelRequest, error?: string) {
    if (!detachModelRequest(request))
      return
    request.controller.abort()
    cancelModelReader(request)
    if (error)
      options.post(request.source, 'models-error', request.id, undefined, error)
  }

  function completeModelRequest(request: ModelRequest, pullId: number) {
    if (!detachModelRequest(request))
      return
    try {
      request.reader?.releaseLock()
    }
    catch {
      // EOF 已确定，锁清理失败不应阻断结束标记。
    }
    options.post(request.source, 'models-complete', request.id, { pullId })
  }

  function armModelIdleTimeout(request: ModelRequest) {
    clearTimeout(request.idleTimer)
    request.idleTimer = setTimeout(() => {
      terminateModelRequest(request, t('plugins.frame.pageDidNotContinueToReadModel'))
    }, MODEL_RESPONSE_IDLE_MS)
  }

  async function handleModelRequest(source: Window, message: PluginBridgeMessage) {
    if (requests.size >= MAXIMUM_MODEL_IN_FLIGHT) {
      options.post(source, 'models-error', message.id, undefined, t('plugins.frame.modelConcurrencyLimit'))
      return
    }
    if (options.requestIds.has(message.id) || requests.has(message.id)) {
      options.post(source, 'models-error', message.id, undefined, t('plugins.frame.duplicateModelRequest'))
      return
    }

    let payload: ReturnType<typeof validateModelRequestPayload>
    try {
      payload = validateModelRequestPayload(message.payload)
    }
    catch (error) {
      options.post(source, 'models-error', message.id, undefined, errorMessage(error))
      return
    }

    const request: ModelRequest = {
      id: message.id,
      source,
      controller: new AbortController(),
      bufferedOffset: 0,
      reading: false,
      nextPullId: 1,
    }
    requests.set(message.id, request)
    request.totalTimer = setTimeout(() => {
      terminateModelRequest(request, t('plugins.frame.modelTimeout'))
    }, MODEL_REQUEST_DEADLINE_MS)

    try {
      const response = await callPluginModelResponses(
        options.target,
        payload.clientKeyId,
        payload.body,
        request.controller.signal,
      )
      if (requests.get(request.id) !== request)
        return
      if (!supportedModelResponse(response))
        throw new Error(t('plugins.frame.unsupportedModelContentType'))

      const hasBody = response.body !== null
      if (response.body)
        request.reader = response.body.getReader()
      const statusText = response.statusText.length <= 256 && !hasControlCharacter(response.statusText)
        ? response.statusText
        : ''
      options.post(source, 'models-response', message.id, {
        status: response.status,
        statusText,
        headers: Array.from(response.headers.entries()),
        hasBody,
      })
      if (!hasBody) {
        detachModelRequest(request)
        return
      }
      armModelIdleTimeout(request)
    }
    catch (error) {
      if (requests.get(request.id) === request)
        terminateModelRequest(request, errorMessage(error, t('plugins.frame.modelRequestFailed')))
    }
  }

  async function handleModelPull(source: Window, message: PluginBridgeMessage) {
    const request = requests.get(message.id)
    if (!request || request.source !== source)
      return
    const pullId = isRecord(message.payload)
      ? message.payload.pullId
      : undefined
    if (!Number.isSafeInteger(pullId) || Number(pullId) < 1) {
      terminateModelRequest(request, t('plugins.frame.invalidPullId'))
      return
    }
    if (request.reading) {
      if (request.activePullId !== pullId)
        terminateModelRequest(request, t('plugins.frame.concurrentPull'))
      return
    }
    if (pullId !== request.nextPullId) {
      terminateModelRequest(request, t('plugins.frame.stalePullId'))
      return
    }
    clearTimeout(request.idleTimer)
    request.idleTimer = undefined
    request.reading = true
    request.activePullId = Number(pullId)
    try {
      while (requests.get(message.id) === request) {
        if (!request.buffered || request.bufferedOffset >= request.buffered.byteLength) {
          request.buffered = undefined
          request.bufferedOffset = 0
          const result = await request.reader?.read()
          if (requests.get(message.id) !== request)
            return
          if (!result || result.done) {
            completeModelRequest(request, Number(pullId))
            return
          }
          if (result.value.byteLength === 0)
            continue
          request.buffered = result.value
        }

        const end = Math.min(request.bufferedOffset + MAXIMUM_MODEL_CHUNK_BYTES, request.buffered.byteLength)
        const chunk = request.buffered.slice(request.bufferedOffset, end)
        request.bufferedOffset = end
        if (request.bufferedOffset >= request.buffered.byteLength) {
          request.buffered = undefined
          request.bufferedOffset = 0
        }
        const body = chunk.buffer
        request.nextPullId += 1
        options.post(source, 'models-chunk', message.id, { pullId, body }, undefined, [body])
        armModelIdleTimeout(request)
        return
      }
    }
    catch (error) {
      if (requests.get(message.id) === request)
        terminateModelRequest(request, errorMessage(error, t('plugins.frame.modelResponseReadFailed')))
    }
    finally {
      request.reading = false
      request.activePullId = undefined
    }
  }

  return {
    has: (id: number) => requests.has(id),
    request: handleModelRequest,
    pull: handleModelPull,
    cancel(source: Window, id: number) {
      const request = requests.get(id)
      if (request?.source === source)
        terminateModelRequest(request)
    },
    dispose() {
      for (const request of requests.values())
        terminateModelRequest(request)
    },
  }
}
