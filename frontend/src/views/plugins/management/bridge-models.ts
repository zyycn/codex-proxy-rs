import type { PluginModelMessage, PluginModelRequest, PluginPageMessageType } from './bridge-contract'
import { hasControlCharacter, MAXIMUM_MODEL_BODY_BYTES, MAXIMUM_MODEL_CHUNK_BYTES, MAXIMUM_MODEL_IN_FLIGHT, MODEL_REQUEST_DEADLINE_MS } from './bridge-contract'

interface ModelOperation {
  id: number
  resolve: (value: Response) => void
  reject: (error: Error) => void
  signal?: AbortSignal
  abort?: () => void
  timer?: number
  controller?: ReadableStreamDefaultController<Uint8Array<ArrayBuffer>>
  responseStarted: boolean
  pullPromise?: Promise<void>
  pullResolve?: () => void
  pullReject?: (error: Error) => void
  pullSequence: number
  pullId?: number
}

// 注入端拥有 Response 与待完成读取，传输身份和页面寿命由页面桥提供
export function createPluginModelBridge(options: {
  nextRequestId: () => number
  isDisposed: () => boolean
  post: (type: PluginPageMessageType, id: number, payload?: unknown) => void
}) {
  const modelOperations = new Map<number, ModelOperation>()
  function abortError(reason: unknown) {
    return reason instanceof Error
      ? reason
      : new DOMException('Model request canceled', 'AbortError')
  }

  function clearModelOperation(operation: ModelOperation) {
    if (modelOperations.get(operation.id) !== operation)
      return
    modelOperations.delete(operation.id)
    clearTimeout(operation.timer)
    if (operation.signal && operation.abort)
      operation.signal.removeEventListener('abort', operation.abort)
  }

  function settleModelPull(operation: ModelOperation, error?: Error) {
    const resolve = operation.pullResolve
    const reject = operation.pullReject
    operation.pullResolve = undefined
    operation.pullReject = undefined
    operation.pullPromise = undefined
    operation.pullId = undefined
    if (error)
      reject?.(error)
    else resolve?.()
  }

  function failModelOperation(operation: ModelOperation, error: Error, notifyParent: boolean) {
    if (modelOperations.get(operation.id) !== operation)
      return
    clearModelOperation(operation)
    settleModelPull(operation, error)
    if (operation.controller) {
      try {
        operation.controller.error(error)
      }
      catch {}
    }
    else {
      operation.reject(error)
    }
    if (notifyParent)
      options.post('models-cancel', operation.id)
  }

  function finishModelOperation(operation: ModelOperation) {
    if (modelOperations.get(operation.id) !== operation)
      return
    clearModelOperation(operation)
    settleModelPull(operation)
  }

  function validateResponseHeaders(value: unknown) {
    if (!Array.isArray(value))
      throw new TypeError('Model response header is invalid')
    for (const entry of value) {
      if (!Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== 'string' || typeof entry[1] !== 'string')
        throw new Error('Model response header is invalid')
    }
    // 与宿主共享浏览器的 Headers 校验，不再按插件身份过滤字段。
    return new Headers(value as [string, string][])
  }

  function handleModelMessage(message: PluginModelMessage) {
    const operation = modelOperations.get(message.id)
    if (!operation)
      return
    if (message.type === 'models-error') {
      const text = typeof message.error === 'string' && message.error
        ? message.error
        : 'Model request failed'
      failModelOperation(operation, new Error(text), false)
      return
    }
    if (message.type === 'models-response') {
      if (operation.controller || operation.responseStarted || !message.payload || typeof message.payload !== 'object') {
        failModelOperation(operation, new Error('Invalid model response protocol'), true)
        return
      }
      try {
        const status = message.payload.status
        const statusText = message.payload.statusText ?? ''
        const hasBody = message.payload.hasBody
        if (typeof status !== 'number' || !Number.isSafeInteger(status) || status < 200 || status > 599 || typeof statusText !== 'string' || statusText.length > 256 || hasControlCharacter(statusText) || typeof hasBody !== 'boolean')
          throw new Error('Model response status is invalid')
        const headers = validateResponseHeaders(message.payload.headers)
        operation.responseStarted = true
        if (!hasBody) {
          finishModelOperation(operation)
          operation.resolve(new Response(null, { status, statusText, headers }))
          return
        }
        const stream = new ReadableStream({
          start(controller) {
            operation.controller = controller
          },
          pull() {
            if (modelOperations.get(operation.id) !== operation)
              return undefined
            if (operation.pullPromise)
              return operation.pullPromise
            operation.pullSequence += 1
            operation.pullId = operation.pullSequence
            operation.pullPromise = new Promise<void>((resolve, reject) => {
              operation.pullResolve = resolve
              operation.pullReject = reject
              options.post('models-pull', operation.id, { pullId: operation.pullId })
            })
            return operation.pullPromise
          },
          cancel() {
            if (modelOperations.get(operation.id) === operation) {
              clearModelOperation(operation)
              settleModelPull(operation)
              options.post('models-cancel', operation.id)
            }
          },
        }, { highWaterMark: 0 })
        operation.resolve(new Response(stream, { status, statusText, headers }))
      }
      catch (error) {
        failModelOperation(operation, error instanceof Error
          ? error
          : new Error('Invalid model response protocol'), true)
      }
      return
    }
    if (!operation.controller || !operation.responseStarted) {
      failModelOperation(operation, new Error('Invalid model response protocol'), true)
      return
    }
    if (message.type === 'models-chunk') {
      const pullId = message.payload?.pullId
      const body = message.payload?.body
      if (!operation.pullPromise || !Number.isSafeInteger(pullId) || pullId !== operation.pullId || !(body instanceof ArrayBuffer) || body.byteLength === 0 || body.byteLength > MAXIMUM_MODEL_CHUNK_BYTES) {
        failModelOperation(operation, new Error('Invalid model response chunk'), true)
        return
      }
      try {
        operation.controller.enqueue(new Uint8Array(body))
        settleModelPull(operation)
      }
      catch (error) {
        failModelOperation(operation, error instanceof Error
          ? error
          : new Error('Model response read failed'), true)
      }
      return
    }
    if (message.type === 'models-complete') {
      const pullId = message.payload?.pullId
      if (!operation.pullPromise || !Number.isSafeInteger(pullId) || pullId !== operation.pullId) {
        failModelOperation(operation, new Error('Model response end tag is invalid'), true)
        return
      }
      try {
        operation.controller.close()
      }
      catch {}
      finishModelOperation(operation)
    }
  }

  function modelResponses(input: PluginModelRequest): Promise<Response> {
    if (options.isDisposed())
      return Promise.reject(new Error('Plugin page is closed'))
    if (!input || typeof input !== 'object' || !input.body || typeof input.body !== 'object' || Array.isArray(input.body))
      return Promise.reject(new TypeError('Responses request body must be a JSON object'))
    if (typeof input.clientKeyId !== 'string' || !input.clientKeyId || input.clientKeyId !== input.clientKeyId.trim() || input.clientKeyId.length > 128 || hasControlCharacter(input.clientKeyId))
      return Promise.reject(new TypeError('clientKeyId is invalid'))
    if (input.signal !== undefined && !(input.signal instanceof AbortSignal))
      return Promise.reject(new TypeError('signal must be AbortSignal'))
    if (input.signal?.aborted)
      return Promise.reject(abortError(input.signal.reason))
    if (modelOperations.size >= MAXIMUM_MODEL_IN_FLIGHT)
      return Promise.reject(new Error('Model request concurrency limit reached'))
    let body
    try {
      body = JSON.stringify(input.body)
    }
    catch {
      return Promise.reject(new TypeError('Responses request body must be JSON serializable'))
    }
    if (typeof body !== 'string' || new TextEncoder().encode(body).byteLength > MAXIMUM_MODEL_BODY_BYTES)
      return Promise.reject(new TypeError('Responses request body exceeds 8 MiB'))
    const id = options.nextRequestId()
    return new Promise((resolve, reject) => {
      const operation: ModelOperation = {
        id,
        resolve,
        reject,
        signal: input.signal,
        abort: undefined,
        timer: undefined,
        controller: undefined,
        responseStarted: false,
        pullPromise: undefined,
        pullResolve: undefined,
        pullReject: undefined,
        pullSequence: 0,
        pullId: undefined,
      }
      operation.abort = () => failModelOperation(operation, abortError(input.signal?.reason), true)
      operation.timer = window.setTimeout(() => failModelOperation(operation, new Error('Model request exceeded 10 minutes'), true), MODEL_REQUEST_DEADLINE_MS)
      modelOperations.set(id, operation)
      input.signal?.addEventListener('abort', operation.abort, { once: true })
      options.post('models-request', id, { clientKeyId: input.clientKeyId, body })
    })
  }

  return {
    responses: modelResponses,
    handle: handleModelMessage,
    dispose() {
      const error = new Error('Plugin page is closed')
      for (const operation of modelOperations.values())
        failModelOperation(operation, error, true)
    },
  }
}
