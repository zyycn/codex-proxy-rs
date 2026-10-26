import type { ShallowRef } from 'vue'
import type { PluginBridgeMessage, PluginModelMessageType } from '../management/bridge-contract'
import type { PluginManagementPage, PluginManagementRoute, PluginManagementView } from '@/api'
import { useEventListener } from '@vueuse/core'
import { computed, nextTick, onScopeDispose, shallowRef, watch } from 'vue'
import {
  callPluginManagementRoute,
  createPluginManagementCallbackTicket,
  pluginManagementCallbackPath,
} from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { ApiError } from '@/api/request'
import { t } from '@/i18n'
import { errorMessage, generateRequestId } from '@/utils/operation'
import { isPluginBridgeRequest, isRecord } from '../management/bridge-contract'
import {
  assemblePluginManagementPage,
  hasControlCharacter,
  MAXIMUM_MANAGEMENT_BODY_BYTES,
  MAXIMUM_MANAGEMENT_IN_FLIGHT,
  MAXIMUM_MANAGEMENT_QUERY_BYTES,
  PLUGIN_MANAGEMENT_BRIDGE,
  PLUGIN_MANAGEMENT_BRIDGE_VERSION,
  readPluginFrameTheme,
} from '../management/frame'
import { createPluginModelStreams } from '../management/model-streams'

interface FrameSession {
  channel: string
  session: string
  controller: AbortController
  view: PluginManagementView
  page: PluginManagementPage
  revoke: () => void
  requestIds: Set<number>
  models: ReturnType<typeof createPluginModelStreams>
  loaded: boolean
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
    current.models.dispose()
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
      const requestIds = new Set<number>()
      const current: FrameSession = {
        channel,
        session: sessionId,
        controller,
        view,
        page,
        revoke: assembled.revoke,
        requestIds,
        models: createPluginModelStreams({
          target: view.target,
          requestIds,
          post: (...args) => postModelMessage(current, ...args),
        }),
        loaded: false,
      }
      session.value = current
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
    message: PluginBridgeMessage,
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
    message: PluginBridgeMessage,
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
    type: PluginModelMessageType,
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

  async function handleBridgeMessage(event: MessageEvent) {
    const current = session.value
    const source = event.source
    if (
      !current
      || source !== options.iframe.value?.contentWindow
      || event.origin !== 'null'
      || !isPluginBridgeRequest(event.data)
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
      void current.models.request(frame, event.data)
      return
    }
    if (event.data.type === 'models-pull') {
      void current.models.pull(frame, event.data)
      return
    }
    if (event.data.type === 'models-cancel') {
      current.models.cancel(frame, event.data.id)
      return
    }
    if (!['request', 'callback-ticket'].includes(event.data.type))
      return
    if (current.requestIds.has(event.data.id) || current.models.has(event.data.id)) {
      postResult(current, frame, event.data.id, false, undefined, t('plugins.frame.duplicateManagementRequest'))
      return
    }
    if (current.requestIds.size >= MAXIMUM_MANAGEMENT_IN_FLIGHT) {
      postResult(current, frame, event.data.id, false, undefined, t('plugins.frame.managementConcurrencyLimit'))
      return
    }
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
