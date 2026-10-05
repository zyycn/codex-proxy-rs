<script setup lang="ts">
import type { PluginManagementPage, PluginManagementRoute, PluginManagementView } from '@/api'

import { BaseButton } from '@codex-proxy/ui'
import { RotateCw } from '@lucide/vue'
import { useEventListener, useResizeObserver } from '@vueuse/core'

import { computed, nextTick, onMounted, onScopeDispose, shallowRef, useTemplateRef, watch } from 'vue'
import {
  callPluginManagementRoute,
  callPluginModelResponses,
  createPluginManagementCallbackTicket,
  pluginManagementCallbackPath,
} from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { ApiError } from '@/api/request'
import { useThemeStore } from '@/stores/modules/theme'
import { errorMessage, generateRequestId } from '@/utils/operation'
import {
  assemblePluginManagementPage,
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
import PluginPageLoading from './PluginPageLoading.vue'

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

const props = defineProps<{
  view: PluginManagementView
  page: PluginManagementPage
}>()

const emit = defineEmits<{
  stale: []
}>()

const themeStore = useThemeStore()
const containerRef = useTemplateRef<HTMLDivElement>('container')
const iframeRef = useTemplateRef<HTMLIFrameElement>('iframe')
const srcdoc = shallowRef('')
const loading = shallowRef(false)
const loadError = shallowRef('')
const contentHeight = shallowRef(0)
const viewportHeight = shallowRef(0)
const frameReady = computed(() => contentHeight.value > 0)
let session: FrameSession | undefined
let loadSequence = 0
let previousViewportHeight = -1

function disposeSession() {
  const current = session
  session = undefined
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
  const sequence = ++loadSequence
  disposeSession()
  loading.value = true
  loadError.value = ''
  const controller = new AbortController()
  const channel = generateRequestId()
  const sessionId = generateRequestId()
  try {
    await nextTick()
    if (sequence !== loadSequence)
      return
    pushLayout()
    const assembled = await assemblePluginManagementPage(
      props.view,
      props.page,
      channel,
      sessionId,
      window.location.origin,
      readPluginFrameTheme(),
      viewportHeight.value,
      controller.signal,
    )
    if (sequence !== loadSequence || controller.signal.aborted) {
      assembled.revoke()
      return
    }
    session = {
      channel,
      session: sessionId,
      controller,
      view: props.view,
      page: props.page,
      revoke: assembled.revoke,
      inFlight: 0,
      requestIds: new Set(),
      modelRequests: new Map(),
      loaded: false,
    }
    srcdoc.value = assembled.srcdoc
  }
  catch (error) {
    if (sequence === loadSequence && !controller.signal.aborted) {
      if (error instanceof ApiError && error.status === 409) {
        loadError.value = '插件页面版本已变更，正在刷新'
        emit('stale')
      }
      else {
        loadError.value = errorMessage(error)
      }
    }
  }
  finally {
    if (sequence === loadSequence)
      loading.value = false
  }
}

function handleFrameLoad() {
  const current = session
  if (!current)
    return
  if (!current.loaded) {
    current.loaded = true
    pushLayout()
    return
  }
  loadError.value = '插件页面尝试离开已隔离的管理文档，已停止加载'
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
  if (session !== current || iframeRef.value?.contentWindow !== source)
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
  const method = typeof payload.method === 'string' ? payload.method.toUpperCase() : ''
  const path = typeof payload.path === 'string' ? payload.path : ''
  return view.routes.find(route => route.method === method && route.path === path)
}

function validateRoutePayload(route: PluginManagementRoute, payload: Record<string, unknown>) {
  const query = payload.query
  const contentType = payload.contentType
  const body = payload.body
  if (typeof query !== 'string' || query.startsWith('?') || query.includes('#') || hasControlCharacter(query))
    throw new Error('查询参数无效')
  if (new TextEncoder().encode(query).byteLength > MAXIMUM_MANAGEMENT_QUERY_BYTES)
    throw new Error('查询参数超过 8 KiB')
  if (!(body instanceof ArrayBuffer) || body.byteLength > MAXIMUM_MANAGEMENT_BODY_BYTES)
    throw new Error('请求正文无效或超过 1 MiB')
  if ((route.method === 'GET' || route.method === 'HEAD') && body.byteLength > 0)
    throw new Error(`${route.method} 路由不接受请求正文`)
  if (contentType !== undefined && typeof contentType !== 'string')
    throw new Error('Content-Type 无效')
  if (typeof contentType === 'string' && !route.requestContentTypes.includes(contentType))
    throw new Error('Content-Type 未在插件路由中注册')
  if (body.byteLength > 0 && typeof contentType !== 'string')
    throw new Error('包含正文的请求必须声明 Content-Type')
  return {
    query,
    contentType: typeof contentType === 'string' ? contentType : undefined,
    body,
  }
}

function hasControlCharacter(value: string) {
  return [...value].some((character) => {
    const code = character.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7F
  })
}

async function handleRouteRequest(
  current: FrameSession,
  source: Window,
  message: BridgeMessage,
) {
  if (!isRecord(message.payload))
    throw new Error('请求参数无效')
  const route = routeForMessage(current.view, message.payload)
  if (!route)
    throw new Error('请求未在当前插件版本中注册')
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
    throw new Error('回调票据参数无效')
  const path = typeof message.payload.path === 'string' ? message.payload.path : ''
  const ttlSeconds = message.payload.ttlSeconds
  if (!current.view.callbacks.some(callback => callback.path === path))
    throw new Error('回调路径未在当前插件版本中注册')
  if (!Number.isSafeInteger(ttlSeconds) || Number(ttlSeconds) < 1 || Number(ttlSeconds) > 600)
    throw new Error('回调票据有效期必须为 1 到 600 秒')
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
  if (session !== current || iframeRef.value?.contentWindow !== source)
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
    throw new Error('模型请求参数无效')
  const clientKeyId = payload.clientKeyId
  const body = payload.body
  if (
    typeof clientKeyId !== 'string'
    || !clientKeyId
    || clientKeyId !== clientKeyId.trim()
    || clientKeyId.length > 128
    || hasControlCharacter(clientKeyId)
  ) {
    throw new Error('clientKeyId 无效')
  }
  if (typeof body !== 'string' || new TextEncoder().encode(body).byteLength > MAXIMUM_MODEL_BODY_BYTES)
    throw new Error('Responses 请求正文无效或超过 8 MiB')
  let parsed: unknown
  try {
    parsed = JSON.parse(body)
  }
  catch {
    throw new Error('Responses 请求正文不是有效 JSON')
  }
  if (!isRecord(parsed))
    throw new Error('Responses 请求正文必须是 JSON 对象')
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
    terminateModelRequest(current, request, '页面 30 秒未继续读取模型响应')
  }, MODEL_RESPONSE_IDLE_MS)
}

async function handleModelRequest(current: FrameSession, source: Window, message: BridgeMessage) {
  if (current.modelRequests.size >= MAXIMUM_MODEL_IN_FLIGHT) {
    postModelMessage(current, source, 'models-error', message.id, undefined, '模型请求已达并发上限')
    return
  }
  if (current.requestIds.has(message.id) || current.modelRequests.has(message.id)) {
    postModelMessage(current, source, 'models-error', message.id, undefined, '模型请求序号重复')
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
    terminateModelRequest(current, request, '模型请求超过 10 分钟')
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
      throw new Error('模型接口返回了不受支持的内容类型')

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
      terminateModelRequest(current, request, errorMessage(error, '模型请求失败'))
  }
}

async function handleModelPull(current: FrameSession, source: Window, message: BridgeMessage) {
  const request = current.modelRequests.get(message.id)
  if (!request || request.source !== source)
    return
  const pullId = isRecord(message.payload) ? message.payload.pullId : undefined
  if (!Number.isSafeInteger(pullId) || Number(pullId) < 1) {
    terminateModelRequest(current, request, '模型响应拉取序号无效')
    return
  }
  if (request.reading) {
    if (request.activePullId !== pullId)
      terminateModelRequest(current, request, '模型响应拉取发生重入')
    return
  }
  if (pullId !== request.nextPullId) {
    terminateModelRequest(current, request, '模型响应拉取序号已失效')
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
      terminateModelRequest(current, request, errorMessage(error, '模型响应读取失败'))
  }
  finally {
    request.reading = false
    request.activePullId = undefined
  }
}

async function handleBridgeMessage(event: MessageEvent) {
  const current = session
  const source = event.source
  if (
    !current
    || source !== iframeRef.value?.contentWindow
    || event.origin !== 'null'
    || !isBridgeMessage(event.data)
    || event.data.channel !== current.channel
    || event.data.session !== current.session
  ) {
    return
  }
  const frame = source as Window
  if (event.data.type === 'resize') {
    const height = isRecord(event.data.payload) ? event.data.payload.height : undefined
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
    postResult(current, frame, event.data.id, false, undefined, '插件管理请求序号重复')
    return
  }
  if (current.inFlight >= MAXIMUM_MANAGEMENT_IN_FLIGHT) {
    postResult(current, frame, event.data.id, false, undefined, '插件管理请求已达并发上限')
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
    if (session !== current || current.controller.signal.aborted)
      return
    if (error instanceof ApiError && error.status === 409) {
      loadError.value = '插件页面版本已变更，正在刷新'
      emit('stale')
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
  const container = containerRef.value
  if (!container)
    return

  // 在 iframe 创建前预留可用空间，计算不依赖插件内容高度。
  let top = container.getBoundingClientRect().top
  let bottomPadding = 0
  for (let parent = container.parentElement; parent; parent = parent.parentElement) {
    top += parent.scrollTop
    bottomPadding += Number.parseFloat(getComputedStyle(parent).paddingBottom) || 0
  }
  const height = Math.max(0, Math.floor(document.documentElement.clientHeight - top - bottomPadding))
  viewportHeight.value = height
  const current = session
  const frame = iframeRef.value
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
  const current = session
  const target = iframeRef.value?.contentWindow
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

useResizeObserver(containerRef, pushLayout)
useEventListener('resize', pushLayout)

watch(
  [
    () => props.view.target.instanceId,
    () => props.view.target.artifactSha256,
    () => props.view.target.revision,
    () => props.page.id,
  ],
  () => void loadPage(),
  { immediate: true },
)
watch(
  () => [themeStore.effectiveTheme, themeStore.themeRevision],
  () => void pushTheme(),
)

onMounted(() => {
  window.addEventListener('message', handleBridgeMessage)
  pushLayout()
})
onScopeDispose(() => {
  loadSequence += 1
  window.removeEventListener('message', handleBridgeMessage)
  disposeSession()
})
</script>

<template>
  <div ref="container" class="relative isolate flex min-w-0 w-full" :style="{ minHeight: `${viewportHeight}px` }" :aria-busy="loading || (!frameReady && !loadError)">
    <div v-if="!loadError && (loading || !frameReady)" class="absolute inset-0 z-10 flex bg-cp-bg-layout">
      <PluginPageLoading />
    </div>
    <div v-if="loadError" class="grid min-h-0 flex-1 place-items-center p-6 text-center">
      <div class="grid max-w-md justify-items-center gap-3">
        <p class="m-0 text-cp font-heavy text-cp-text">
          插件页面无法加载
        </p>
        <p class="m-0 text-xs leading-relaxed text-cp-text-secondary">
          {{ loadError }}
        </p>
        <BaseButton size="sm" variant="secondary" @click="loadPage">
          <template #icon>
            <RotateCw class="size-3.5" />
          </template>
          重试
        </BaseButton>
      </div>
    </div>
    <iframe
      v-else-if="srcdoc"
      ref="iframe"
      :key="session?.channel"
      :srcdoc="srcdoc"
      :title="`${view.name}：${page.title}`"
      class="block w-full border-0 bg-transparent"
      :style="{ height: `${Math.max(viewportHeight, contentHeight)}px` }"
      :inert="!frameReady"
      :aria-hidden="!frameReady"
      sandbox="allow-scripts"
      referrerpolicy="no-referrer"
      @load="handleFrameLoad"
    />
  </div>
</template>
