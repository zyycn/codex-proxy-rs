import type { ShallowRef } from 'vue'
import type { PluginManagementPage, PluginManagementRoute, PluginManagementView } from '@/api'
import { useEventListener } from '@vueuse/core'
import { computed, nextTick, onScopeDispose, shallowRef, watch } from 'vue'
import {
  callPluginManagementRoute,
  callPluginModelResponses,
  createPluginManagementCallbackTicket,
  pluginManagementCallbackPath,
} from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { ApiError } from '@/api/request'
import { t } from '@/i18n'
import { errorMessage, generateRequestId } from '@/utils/operation'
import {
  assemblePluginManagementPage,
  hasControlCharacter,
  MAXIMUM_MANAGEMENT_BODY_BYTES,
  MAXIMUM_MANAGEMENT_IN_FLIGHT,
  MAXIMUM_MANAGEMENT_QUERY_BYTES,
  MAXIMUM_MODEL_BODY_BYTES,
  MAXIMUM_MODEL_CHUNK_BYTES,
  MAXIMUM_MODEL_IN_FLIGHT,
  MODEL_REQUEST_DEADLINE_MS,
  MODEL_RESPONSE_IDLE_MS,
  PLUGIN_MANAGEMENT_BRIDGE,
  PLUGIN_MANAGEMENT_BRIDGE_VERSION,
  readPluginFrameTheme,
} from '../management/frame'

interface BridgeMessage {
  target: string
  version: number
  channel: string
  session: string
  type: string
  id: number
  payload?: unknown
}

interface FrameSession {
  channel: string
  session: string
  controller: AbortController
  view: PluginManagementView
  page: PluginManagementPage
  revoke: () => void
  inFlight: number
  requestIds: Set<number>
  modelRequests: Map<number, ModelRequest>
  loaded: boolean
}

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

export function usePluginFrameSession(options: {
  view: () => PluginManagementView
  page: () => PluginManagementPage
  iframe: Readonly<ShallowRef<HTMLIFrameElement | null>>
  viewportHeight: () => number
  onStale: () => void
}) {
  const srcdoc = shallowRef('')
  const loading = shallowRef(false)
  const loadError = shallowRef('')
  const contentHeight = shallowRef(0)
  const session = shallowRef<FrameSession>()
  let loadController: AbortController | undefined
  let previousViewportHeight = -1

  function disposeSession() {
    loadController?.abort()
    loadController = undefined
    const current = session.value
    session.value = undefined
    srcdoc.value = ''
    contentHeight.value = 0
    previousViewportHeight = -1
    if (!current)
      return
    current.controller.abort()
    for (const request of current.modelRequests.values())
      terminateModelRequest(current, request)
    current.revoke()
  }

  async function loadPage() {
    disposeSession()
    loading.value = true
    loadError.value = ''
    const controller = new AbortController()
    loadController = controller
    const view = options.view()
    const page = options.page()
    const channel = generateRequestId()
    const sessionId = generateRequestId()
    try {
      await nextTick()
      if (controller.signal.aborted)
        return
      const assembled = await assemblePluginManagementPage(
        view,
        page,
        channel,
        sessionId,
        window.location.origin,
        readPluginFrameTheme(),
        options.viewportHeight(),
        controller.signal,
      )
      if (controller.signal.aborted) {
        assembled.revoke()
        return
      }
      session.value = {
        channel,
        session: sessionId,
        controller,
        view,
        page,
        revoke: assembled.revoke,
        inFlight: 0,
        requestIds: new Set(),
        modelRequests: new Map(),
        loaded: false,
      }
      srcdoc.value = assembled.srcdoc
    }
    catch (error) {
      if (!controller.signal.aborted) {
        // 一个资源加载失败时，同时停止仍在运行的其他资源请求。
        controller.abort()
        if (error instanceof ApiError && error.status === 409) {
          loadError.value = t('plugins.frame.versionChanged')
          options.onStale()
        }
        else {
          loadError.value = errorMessage(error)
        }
      }
    }
    finally {
      if (loadController === controller) {
        loadController = undefined
        loading.value = false
      }
    }
  }

  function handleFrameLoad() {
    const current = session.value
    if (!current)
      return
    if (!current.loaded) {
      current.loaded = true
      pushLayout()
      return
    }
    loadError.value = t('plugins.frame.navigationBlocked')
    disposeSession()
  }

  function isBridgeMessage(value: unknown): value is BridgeMessage {
    if (!value || typeof value !== 'object')
      return false
    const message = value as Record<string, unknown>
    return message.target === PLUGIN_MANAGEMENT_BRIDGE
      && message.version === PLUGIN_MANAGEMENT_BRIDGE_VERSION
      && typeof message.channel === 'string'
      && typeof message.session === 'string'
      && typeof message.type === 'string'
      && Number.isSafeInteger(message.id)
      && Number(message.id) > 0
  }

  function isRecord(value: unknown): value is Record<string, unknown> {
    return Boolean(value && typeof value === 'object' && !Array.isArray(value))
  }

  function postResult(
    current: FrameSession,
    source: Window,
    id: number,
    ok: boolean,
    payload?: unknown,
    error?: string,
    transfer: Transferable[] = [],
  ) {
    if (session.value !== current || options.iframe.value?.contentWindow !== source)
      return
    source.postMessage({
      target: PLUGIN_MANAGEMENT_BRIDGE,
      version: PLUGIN_MANAGEMENT_BRIDGE_VERSION,
      channel: current.channel,
      session: current.session,
      type: 'result',
      id,
      ok,
      payload,
      error,
    }, '*', transfer)
  }

  function routeForMessage(view: PluginManagementView, payload: Record<string, unknown>) {
    const method = typeof payload.method === 'string'
      ? payload.method.toUpperCase()
      : ''
    const path = typeof payload.path === 'string'
      ? payload.path
      : ''
    return view.routes.find(route => route.method === method && route.path === path)
  }

  function validateRoutePayload(route: PluginManagementRoute, payload: Record<string, unknown>) {
    const query = payload.query
    const contentType = payload.contentType
    const body = payload.body
    if (typeof query !== 'string' || query.startsWith('?') || query.includes('#') || hasControlCharacter(query))
      throw new Error(t('plugins.frame.invalidQueryParameter'))
    if (new TextEncoder().encode(query).byteLength > MAXIMUM_MANAGEMENT_QUERY_BYTES)
      throw new Error(t('plugins.frame.queryParametersExceedKib'))
    if (!(body instanceof ArrayBuffer) || body.byteLength > MAXIMUM_MANAGEMENT_BODY_BYTES)
      throw new Error(t('plugins.frame.invalidBody'))
    if ((route.method === 'GET' || route.method === 'HEAD') && body.byteLength > 0)
      throw new Error(t('plugins.frame.routesDoNotAcceptRequestBody', { method: route.method }))
    if (contentType !== undefined && typeof contentType !== 'string')
      throw new Error(t('plugins.frame.contentTypeIsInvalid'))
    if (typeof contentType === 'string' && !route.requestContentTypes.includes(contentType))
      throw new Error(t('plugins.frame.unregisteredContentType'))
    if (body.byteLength > 0 && typeof contentType !== 'string')
      throw new Error(t('plugins.frame.contentTypeRequired'))
    return {
      query,
      contentType: typeof contentType === 'string'
        ? contentType
        : undefined,
      body,
    }
  }

  async function handleRouteRequest(
    current: FrameSession,
    source: Window,
    message: BridgeMessage,
  ) {
    if (!isRecord(message.payload))
      throw new Error(t('plugins.frame.invalidRequestParameters'))
    const route = routeForMessage(current.view, message.payload)
    if (!route)
      throw new Error(t('plugins.frame.unregisteredRequest'))
    const payload = validateRoutePayload(route, message.payload)
    const response = await callPluginManagementRoute(current.view.target, {
      method: route.method,
      path: route.path,
      query: payload.query,
      contentType: payload.contentType,
      body: payload.body,
    }, { signal: current.controller.signal, silent: true })
    const body = response.body
    postResult(current, source, message.id, true, {
      status: response.status,
      contentType: response.contentType,
      body,
    }, undefined, [body])
  }

  async function handleCallbackTicket(
    current: FrameSession,
    source: Window,
    message: BridgeMessage,
  ) {
    if (!isRecord(message.payload))
      throw new Error(t('plugins.frame.invalidCallbackTicket'))
    const path = typeof message.payload.path === 'string'
      ? message.payload.path
      : ''
    const ttlSeconds = message.payload.ttlSeconds
    if (!current.view.callbacks.some(callback => callback.path === path))
      throw new Error(t('plugins.frame.unregisteredCallback'))
    if (!Number.isSafeInteger(ttlSeconds) || Number(ttlSeconds) < 1 || Number(ttlSeconds) > 600)
      throw new Error(t('plugins.frame.invalidTicketLifetime'))
    const ticket = await createPluginManagementCallbackTicket(current.view.target, {
      path,
      ttlSeconds: Number(ttlSeconds),
    }, { signal: current.controller.signal, silent: true })
    const callbackPath = `${API_BASE_URL}${pluginManagementCallbackPath(current.view.target, path, ticket.state)}`
    postResult(current, source, message.id, true, {
      state: ticket.state,
      expiresAtMs: ticket.expiresAtMs,
      callbackUrl: new URL(callbackPath, window.location.origin).toString(),
    })
  }

  function postModelMessage(
    current: FrameSession,
    source: Window,
    type: 'models-response' | 'models-chunk' | 'models-complete' | 'models-error',
    id: number,
    payload?: unknown,
    error?: string,
    transfer: Transferable[] = [],
  ) {
    if (session.value !== current || options.iframe.value?.contentWindow !== source)
      return
    source.postMessage({
      target: PLUGIN_MANAGEMENT_BRIDGE,
      version: PLUGIN_MANAGEMENT_BRIDGE_VERSION,
      channel: current.channel,
      session: current.session,
      type,
      id,
      payload,
      error,
    }, '*', transfer)
  }

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

  function modelResponseHeaders(response: Response) {
    // 完整转交浏览器可读取的响应头；浏览器自身的 Headers 合同仍然适用。
    return Array.from(response.headers.entries())
  }

  function supportedModelResponse(response: Response) {
    if (!response.body)
      return true
    const contentType = response.headers.get('content-type')?.split(';', 1)[0]?.trim().toLowerCase()
    return contentType === 'application/json'
      || contentType === 'text/event-stream'
      || Boolean(contentType?.endsWith('+json'))
  }

  function detachModelRequest(current: FrameSession, request: ModelRequest) {
    if (current.modelRequests.get(request.id) !== request)
      return false
    current.modelRequests.delete(request.id)
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

  function terminateModelRequest(current: FrameSession, request: ModelRequest, error?: string) {
    if (!detachModelRequest(current, request))
      return
    request.controller.abort()
    cancelModelReader(request)
    if (error)
      postModelMessage(current, request.source, 'models-error', request.id, undefined, error)
  }

  function completeModelRequest(current: FrameSession, request: ModelRequest, pullId: number) {
    if (!detachModelRequest(current, request))
      return
    try {
      request.reader?.releaseLock()
    }
    catch {
      // EOF 已确定，锁清理失败不应阻断结束标记。
    }
    postModelMessage(current, request.source, 'models-complete', request.id, { pullId })
  }

  function armModelIdleTimeout(current: FrameSession, request: ModelRequest) {
    clearTimeout(request.idleTimer)
    request.idleTimer = setTimeout(() => {
      terminateModelRequest(current, request, t('plugins.frame.pageDidNotContinueToReadModel'))
    }, MODEL_RESPONSE_IDLE_MS)
  }

  async function handleModelRequest(current: FrameSession, source: Window, message: BridgeMessage) {
    if (current.modelRequests.size >= MAXIMUM_MODEL_IN_FLIGHT) {
      postModelMessage(current, source, 'models-error', message.id, undefined, t('plugins.frame.modelConcurrencyLimit'))
      return
    }
    if (current.requestIds.has(message.id) || current.modelRequests.has(message.id)) {
      postModelMessage(current, source, 'models-error', message.id, undefined, t('plugins.frame.duplicateModelRequest'))
      return
    }

    let payload: ReturnType<typeof validateModelRequestPayload>
    try {
      payload = validateModelRequestPayload(message.payload)
    }
    catch (error) {
      postModelMessage(current, source, 'models-error', message.id, undefined, errorMessage(error))
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
    current.modelRequests.set(message.id, request)
    request.totalTimer = setTimeout(() => {
      terminateModelRequest(current, request, t('plugins.frame.modelTimeout'))
    }, MODEL_REQUEST_DEADLINE_MS)

    try {
      const response = await callPluginModelResponses(
        current.view.target,
        payload.clientKeyId,
        payload.body,
        request.controller.signal,
      )
      if (current.modelRequests.get(request.id) !== request)
        return
      if (!supportedModelResponse(response))
        throw new Error(t('plugins.frame.unsupportedModelContentType'))

      const hasBody = response.body !== null
      if (response.body)
        request.reader = response.body.getReader()
      const statusText = response.statusText.length <= 256 && !hasControlCharacter(response.statusText)
        ? response.statusText
        : ''
      postModelMessage(current, source, 'models-response', message.id, {
        status: response.status,
        statusText,
        headers: modelResponseHeaders(response),
        hasBody,
      })
      if (!hasBody) {
        detachModelRequest(current, request)
        return
      }
      armModelIdleTimeout(current, request)
    }
    catch (error) {
      if (current.modelRequests.get(request.id) === request)
        terminateModelRequest(current, request, errorMessage(error, t('plugins.frame.modelRequestFailed')))
    }
  }

  async function handleModelPull(current: FrameSession, source: Window, message: BridgeMessage) {
    const request = current.modelRequests.get(message.id)
    if (!request || request.source !== source)
      return
    const pullId = isRecord(message.payload)
      ? message.payload.pullId
      : undefined
    if (!Number.isSafeInteger(pullId) || Number(pullId) < 1) {
      terminateModelRequest(current, request, t('plugins.frame.invalidPullId'))
      return
    }
    if (request.reading) {
      if (request.activePullId !== pullId)
        terminateModelRequest(current, request, t('plugins.frame.concurrentPull'))
      return
    }
    if (pullId !== request.nextPullId) {
      terminateModelRequest(current, request, t('plugins.frame.stalePullId'))
      return
    }
    clearTimeout(request.idleTimer)
    request.idleTimer = undefined
    request.reading = true
    request.activePullId = Number(pullId)
    try {
      while (current.modelRequests.get(message.id) === request) {
        if (!request.buffered || request.bufferedOffset >= request.buffered.byteLength) {
          request.buffered = undefined
          request.bufferedOffset = 0
          const result = await request.reader?.read()
          if (current.modelRequests.get(message.id) !== request)
            return
          if (!result || result.done) {
            completeModelRequest(current, request, Number(pullId))
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
        postModelMessage(current, source, 'models-chunk', message.id, { pullId, body }, undefined, [body])
        armModelIdleTimeout(current, request)
        return
      }
    }
    catch (error) {
      if (current.modelRequests.get(message.id) === request)
        terminateModelRequest(current, request, errorMessage(error, t('plugins.frame.modelResponseReadFailed')))
    }
    finally {
      request.reading = false
      request.activePullId = undefined
    }
  }

  async function handleBridgeMessage(event: MessageEvent) {
    const current = session.value
    const source = event.source
    if (
      !current
      || source !== options.iframe.value?.contentWindow
      || event.origin !== 'null'
      || !isBridgeMessage(event.data)
      || event.data.channel !== current.channel
      || event.data.session !== current.session
    ) {
      return
    }
    const frame = source as Window
    if (event.data.type === 'resize') {
      const height = isRecord(event.data.payload)
        ? event.data.payload.height
        : undefined
      if (typeof height === 'number' && Number.isSafeInteger(height) && height > 0 && height <= 1_000_000)
        contentHeight.value = height
      return
    }
    if (event.data.type === 'models-request') {
      void handleModelRequest(current, frame, event.data)
      return
    }
    if (event.data.type === 'models-pull') {
      void handleModelPull(current, frame, event.data)
      return
    }
    if (event.data.type === 'models-cancel') {
      const request = current.modelRequests.get(event.data.id)
      if (request?.source === frame)
        terminateModelRequest(current, request)
      return
    }
    if (!['request', 'callback-ticket'].includes(event.data.type))
      return
    if (current.requestIds.has(event.data.id) || current.modelRequests.has(event.data.id)) {
      postResult(current, frame, event.data.id, false, undefined, t('plugins.frame.duplicateManagementRequest'))
      return
    }
    if (current.inFlight >= MAXIMUM_MANAGEMENT_IN_FLIGHT) {
      postResult(current, frame, event.data.id, false, undefined, t('plugins.frame.managementConcurrencyLimit'))
      return
    }
    current.inFlight += 1
    current.requestIds.add(event.data.id)
    try {
      if (event.data.type === 'request')
        await handleRouteRequest(current, frame, event.data)
      else await handleCallbackTicket(current, frame, event.data)
    }
    catch (error) {
      if (session.value !== current || current.controller.signal.aborted)
        return
      if (error instanceof ApiError && error.status === 409) {
        loadError.value = t('plugins.frame.versionChanged')
        options.onStale()
        disposeSession()
        return
      }
      postResult(current, frame, event.data.id, false, undefined, errorMessage(error))
    }
    finally {
      current.requestIds.delete(event.data.id)
      current.inFlight = Math.max(0, current.inFlight - 1)
    }
  }

  function pushLayout() {
    const height = options.viewportHeight()
    const current = session.value
    const frame = options.iframe.value
    if (!current?.loaded || !frame?.contentWindow)
      return
    if (height === previousViewportHeight)
      return
    previousViewportHeight = height
    frame.contentWindow.postMessage({
      target: PLUGIN_MANAGEMENT_BRIDGE,
      version: PLUGIN_MANAGEMENT_BRIDGE_VERSION,
      channel: current.channel,
      session: current.session,
      type: 'layout',
      payload: { viewportHeight: height },
    }, '*')
  }

  async function pushTheme() {
    await nextTick()
    const current = session.value
    const target = options.iframe.value?.contentWindow
    if (!current || !target)
      return
    target.postMessage({
      target: PLUGIN_MANAGEMENT_BRIDGE,
      version: PLUGIN_MANAGEMENT_BRIDGE_VERSION,
      channel: current.channel,
      session: current.session,
      type: 'theme',
      payload: readPluginFrameTheme(),
    }, '*')
    pushLayout()
  }

  useEventListener('message', handleBridgeMessage)
  onScopeDispose(disposeSession)

  watch(
    [
      () => options.view().target.instanceId,
      () => options.view().target.artifactSha256,
      () => options.view().target.revision,
      () => options.page().id,
    ],
    () => void loadPage(),
    { immediate: true },
  )

  const frameKey = computed(() => session.value?.channel)
  return {
    srcdoc,
    loading,
    loadError,
    contentHeight,
    frameKey,
    loadPage,
    handleFrameLoad,
    pushLayout,
    pushTheme,
  }
}
