import type { PluginBridgeInitial } from './bridge-contract'
import { hasControlCharacter, MAXIMUM_MANAGEMENT_BODY_BYTES, MAXIMUM_MANAGEMENT_IN_FLIGHT, MAXIMUM_MANAGEMENT_QUERY_BYTES, MAXIMUM_MODEL_BODY_BYTES, MAXIMUM_MODEL_CHUNK_BYTES, MAXIMUM_MODEL_IN_FLIGHT, MODEL_REQUEST_DEADLINE_MS } from './bridge-contract'

interface PendingOperation {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
  timer: number
}

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

interface ModelMessage {
  id: number
  type: string
  payload?: Record<string, unknown>
  error?: unknown
}

interface ManagementRequest {
  method: string
  path: string
  query?: string
  contentType?: string
  body?: string | ArrayBuffer | ArrayBufferView
}

interface ModelRequest {
  clientKeyId: string
  body: Record<string, unknown>
  signal?: AbortSignal
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export default function installPluginBridge(initial: PluginBridgeInitial) {
  const pending = new Map<number, PendingOperation>()
  const modelOperations = new Map<number, ModelOperation>()
  let sequence = 0
  let resizeObserver: ResizeObserver | undefined
  let resizeFrame = 0
  let previousHeight = 0
  let disposed = false
  let currentTheme = initial.theme.name
  const maximumBodyBytes = MAXIMUM_MANAGEMENT_BODY_BYTES
  const maximumQueryBytes = MAXIMUM_MANAGEMENT_QUERY_BYTES
  const maximumInFlight = MAXIMUM_MANAGEMENT_IN_FLIGHT
  const maximumModelBodyBytes = MAXIMUM_MODEL_BODY_BYTES
  const maximumModelChunkBytes = MAXIMUM_MODEL_CHUNK_BYTES
  const maximumModelInFlight = MAXIMUM_MODEL_IN_FLIGHT
  const modelRequestDeadlineMs = MODEL_REQUEST_DEADLINE_MS

  function applyLayout(height: unknown) {
    if (typeof height === 'number' && Number.isSafeInteger(height) && height >= 0) {
      document.documentElement.style.setProperty('--cp-plugin-viewport-height', `${height}px`)
    }
  }

  function applyTheme(theme: unknown) {
    if (!isRecord(theme) || (theme.name !== 'light' && theme.name !== 'dark') || !theme.tokens || typeof theme.tokens !== 'object')
      return
    const root = document.documentElement
    root.dataset.theme = theme.name
    root.style.colorScheme = theme.name
    for (const [name, value] of Object.entries(theme.tokens)) {
      if (typeof name === 'string' && name.startsWith('--cp-') && typeof value === 'string')
        root.style.setProperty(name, value)
    }
    currentTheme = theme.name
    window.dispatchEvent(new CustomEvent('codex-proxy-themechange', {
      detail: { theme: currentTheme },
    }))
  }

  async function bodyBytes(body: unknown): Promise<ArrayBuffer> {
    if (body == null)
      return new ArrayBuffer(0)
    if (typeof body === 'string')
      return new TextEncoder().encode(body).buffer
    if (body instanceof ArrayBuffer)
      return body.slice(0)
    if (ArrayBuffer.isView(body))
      return new Uint8Array(body.buffer, body.byteOffset, body.byteLength).slice().buffer
    throw new TypeError('Request body must be a string, ArrayBuffer, or TypedArray')
  }

  function nextRequestId() {
    sequence += 1
    if (!Number.isSafeInteger(sequence))
      throw new RangeError('Plugin page request IDs exhausted')
    return sequence
  }

  function post(type: string, id: number, payload?: unknown, transfer: Transferable[] = []) {
    parent.postMessage({
      target: initial.target,
      version: initial.version,
      channel: initial.channel,
      session: initial.session,
      type,
      id,
      payload,
    }, initial.parentOrigin, transfer)
  }

  function invoke(kind: string, payload: unknown, transfer: Transferable[] = []): Promise<unknown> {
    if (disposed)
      return Promise.reject(new Error('Plugin page is closed'))
    if (pending.size >= maximumInFlight)
      return Promise.reject(new Error('Plugin management concurrency limit reached'))
    const id = nextRequestId()
    return new Promise((resolve, reject) => {
      const timer = window.setTimeout(() => {
        pending.delete(id)
        reject(new Error('Plugin management request timed out'))
      }, kind === 'request'
        ? initial.managementTimeoutMs
        : initial.apiTimeoutMs)
      pending.set(id, { resolve, reject, timer })
      post(kind, id, payload, transfer)
    })
  }

  function reportContentHeight() {
    // 不读 documentElement.scrollHeight，避免当前 iframe 高度阻止内容收缩。
    const height = Math.max(1, Math.ceil(document.body.getBoundingClientRect().height))
    if (height === previousHeight)
      return
    previousHeight = height
    post('resize', nextRequestId(), { height })
  }

  async function observeContentHeight() {
    // load 后先触发布局，让实际使用的字体进入加载集合，再等待字体与布局就绪。
    document.body.getBoundingClientRect()
    await document.fonts.ready
    if (disposed)
      return
    // 首次上报不依赖渲染回调，后台或视口外的 iframe 也能完成初始化。
    reportContentHeight()
    resizeObserver = new ResizeObserver(() => {
      if (resizeFrame)
        return
      resizeFrame = requestAnimationFrame(() => {
        resizeFrame = 0
        reportContentHeight()
      })
    })
    resizeObserver.observe(document.body, { box: 'border-box' })
  }

  window.addEventListener('load', observeContentHeight, { once: true })

  async function request(input: ManagementRequest) {
    if (!input || typeof input !== 'object')
      throw new TypeError('Invalid plugin management request')
    const method = String(input.method || '').toUpperCase()
    const path = String(input.path || '')
    const query = input.query == null
      ? ''
      : String(input.query)
    const contentType = input.contentType == null
      ? undefined
      : String(input.contentType)
    if (new TextEncoder().encode(query).byteLength > maximumQueryBytes || query.includes('#') || query.startsWith('?'))
      throw new TypeError('Invalid plugin management query parameters')
    const body = await bodyBytes(input.body)
    if (body.byteLength > maximumBodyBytes)
      throw new TypeError('Plugin management request body exceeds 1 MiB')
    return invoke('request', {
      method,
      path,
      query,
      contentType,
      body,
    }, [body])
  }

  function callbackTicket(input: { path: string, ttlSeconds: number }) {
    if (!input || typeof input !== 'object')
      return Promise.reject(new TypeError('Invalid plugin callback ticket request'))
    return invoke('callback-ticket', {
      path: String(input.path || ''),
      ttlSeconds: Number(input.ttlSeconds),
    })
  }

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
      post('models-cancel', operation.id)
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

  function handleModelMessage(message: ModelMessage) {
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
              post('models-pull', operation.id, { pullId: operation.pullId })
            })
            return operation.pullPromise
          },
          cancel() {
            if (modelOperations.get(operation.id) === operation) {
              clearModelOperation(operation)
              settleModelPull(operation)
              post('models-cancel', operation.id)
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
      if (!operation.pullPromise || !Number.isSafeInteger(pullId) || pullId !== operation.pullId || !(body instanceof ArrayBuffer) || body.byteLength === 0 || body.byteLength > maximumModelChunkBytes) {
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

  function modelResponses(input: ModelRequest): Promise<Response> {
    if (disposed)
      return Promise.reject(new Error('Plugin page is closed'))
    if (!input || typeof input !== 'object' || !input.body || typeof input.body !== 'object' || Array.isArray(input.body))
      return Promise.reject(new TypeError('Responses request body must be a JSON object'))
    if (typeof input.clientKeyId !== 'string' || !input.clientKeyId || input.clientKeyId !== input.clientKeyId.trim() || input.clientKeyId.length > 128 || hasControlCharacter(input.clientKeyId))
      return Promise.reject(new TypeError('clientKeyId is invalid'))
    if (input.signal !== undefined && !(input.signal instanceof AbortSignal))
      return Promise.reject(new TypeError('signal must be AbortSignal'))
    if (input.signal?.aborted)
      return Promise.reject(abortError(input.signal.reason))
    if (modelOperations.size >= maximumModelInFlight)
      return Promise.reject(new Error('Model request concurrency limit reached'))
    let body
    try {
      body = JSON.stringify(input.body)
    }
    catch {
      return Promise.reject(new TypeError('Responses request body must be JSON serializable'))
    }
    if (typeof body !== 'string' || new TextEncoder().encode(body).byteLength > maximumModelBodyBytes)
      return Promise.reject(new TypeError('Responses request body exceeds 8 MiB'))
    const id = nextRequestId()
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
      operation.timer = window.setTimeout(() => failModelOperation(operation, new Error('Model request exceeded 10 minutes'), true), modelRequestDeadlineMs)
      modelOperations.set(id, operation)
      input.signal?.addEventListener('abort', operation.abort, { once: true })
      post('models-request', id, { clientKeyId: input.clientKeyId, body })
    })
  }

  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || !(event.target instanceof Element))
      return
    const submitter = event.target.closest('button, input')
    if (!(submitter instanceof HTMLButtonElement || submitter instanceof HTMLInputElement))
      return
    const type = submitter.type.toLowerCase()
    if (type !== 'submit' && type !== 'image')
      return
    const form = submitter.form
    if (!form)
      return
    event.preventDefault()
    queueMicrotask(() => {
      if (!form.isConnected || !submitter.isConnected)
        return
      if (!submitter.formNoValidate && !form.reportValidity())
        return
      form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true, submitter }))
    })
  })

  window.addEventListener('message', (event) => {
    if (event.source !== parent || event.origin !== initial.parentOrigin)
      return
    const message: unknown = event.data
    if (!isRecord(message) || message.target !== initial.target || message.version !== initial.version || message.channel !== initial.channel || message.session !== initial.session)
      return
    if (message.type === 'theme') {
      applyTheme(message.payload)
      return
    }
    if (message.type === 'layout') {
      applyLayout(isRecord(message.payload)
        ? message.payload.viewportHeight
        : undefined)
      return
    }
    if (typeof message.id !== 'number' || !Number.isSafeInteger(message.id) || message.id < 1)
      return
    if (message.type === 'models-response' || message.type === 'models-chunk' || message.type === 'models-complete' || message.type === 'models-error') {
      handleModelMessage({
        id: message.id,
        type: message.type,
        error: message.error,
        payload: isRecord(message.payload)
          ? message.payload
          : undefined,
      })
      return
    }
    if (message.type !== 'result')
      return
    const operation = pending.get(message.id)
    if (!operation)
      return
    pending.delete(message.id)
    clearTimeout(operation.timer)
    if (message.ok) {
      operation.resolve(message.payload)
    }
    else {
      operation.reject(new Error(typeof message.error === 'string'
        ? message.error
        : 'Plugin management request failed'))
    }
  })

  window.addEventListener('pagehide', () => {
    disposed = true
    resizeObserver?.disconnect()
    cancelAnimationFrame(resizeFrame)
    const error = new Error('Plugin page is closed')
    for (const operation of pending.values()) {
      clearTimeout(operation.timer)
      operation.reject(error)
    }
    pending.clear()
    for (const operation of [
      ...modelOperations.values(),
    ])
      failModelOperation(operation, error, true)
  }, { once: true })

  const api = {
    version: initial.version,
    plugin: Object.freeze(initial.plugin),
    page: Object.freeze(initial.page),
    get theme() { return currentTheme },
    request,
    callbackTicket,
    models: Object.freeze({ responses: modelResponses }),
    resourceUrl(path: string) {
      const key = String(path)
      if (!Object.hasOwn(initial.resourceUrls, key))
        throw new Error('Resource is unregistered or cannot be loaded as a page subresource')
      const value = initial.resourceUrls[key]
      return value
    },
  }
  Object.defineProperty(window, 'codexProxyPlugin', {
    value: Object.freeze(api),
    configurable: false,
    writable: false,
  })
  applyTheme(initial.theme)
  applyLayout(initial.viewportHeight)
}
