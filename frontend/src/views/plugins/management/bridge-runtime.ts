import type { PluginBridgeInitial, PluginCallbackTicket, PluginHostBridge, PluginManagementReply, PluginManagementRequest, PluginPageMessageType } from './bridge-contract'
import { isRecord, MAXIMUM_MANAGEMENT_BODY_BYTES, MAXIMUM_MANAGEMENT_IN_FLIGHT, MAXIMUM_MANAGEMENT_QUERY_BYTES } from './bridge-contract'
import { createPluginModelBridge } from './bridge-models'

interface PendingOperation {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
  timer: number
}

export default function installPluginBridge(initial: PluginBridgeInitial) {
  const pending = new Map<number, PendingOperation>()
  let sequence = 0
  let resizeObserver: ResizeObserver | undefined
  let resizeFrame = 0
  let previousHeight = 0
  let disposed = false
  let currentTheme = initial.theme.name
  const maximumBodyBytes = MAXIMUM_MANAGEMENT_BODY_BYTES
  const maximumQueryBytes = MAXIMUM_MANAGEMENT_QUERY_BYTES
  const maximumInFlight = MAXIMUM_MANAGEMENT_IN_FLIGHT

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

  function post(type: PluginPageMessageType, id: number, payload?: unknown, transfer: Transferable[] = []) {
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

  const modelBridge = createPluginModelBridge({
    nextRequestId,
    post,
    isDisposed: () => disposed,
  })

  function invoke<T>(kind: 'request' | 'callback-ticket', payload: unknown, transfer: Transferable[] = []): Promise<T> {
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
      pending.set(id, {
        resolve: value => resolve(value as T),
        reject,
        timer,
      })
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

  async function request(input: PluginManagementRequest) {
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
    return invoke<PluginManagementReply>('request', {
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
    return invoke<PluginCallbackTicket>('callback-ticket', {
      path: String(input.path || ''),
      ttlSeconds: Number(input.ttlSeconds),
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
      modelBridge.handle({
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
    modelBridge.dispose()
  }, { once: true })

  const api: PluginHostBridge = {
    version: initial.version,
    plugin: Object.freeze(initial.plugin),
    page: Object.freeze(initial.page),
    get theme() { return currentTheme },
    request,
    callbackTicket,
    models: Object.freeze({ responses: modelBridge.responses }),
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
