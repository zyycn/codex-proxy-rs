import type {
  PluginManagementPage,
  PluginManagementResource,
  PluginManagementView,
} from '@/api'
import { THEME_TOKEN_NAMES } from '@codex-proxy/ui/theme'
import { generate, parse, walk } from 'css-tree'

import { getPluginManagementResource, PLUGIN_MANAGEMENT_TIMEOUT_MS } from '@/api'
import { API_TIMEOUT_MS } from '@/api/constants'

export const PLUGIN_MANAGEMENT_BRIDGE = 'codex-proxy-plugin-management'
export const PLUGIN_MANAGEMENT_BRIDGE_VERSION = 2
export const MAXIMUM_MANAGEMENT_BODY_BYTES = 1024 * 1024
export const MAXIMUM_MANAGEMENT_QUERY_BYTES = 8192
export const MAXIMUM_MANAGEMENT_IN_FLIGHT = 8
export const MAXIMUM_MODEL_BODY_BYTES = 8 * 1024 * 1024
export const MAXIMUM_MODEL_CHUNK_BYTES = 64 * 1024
export const MAXIMUM_MODEL_IN_FLIGHT = 4
export const MODEL_REQUEST_DEADLINE_MS = 10 * 60 * 1000
export const MODEL_RESPONSE_IDLE_MS = 30 * 1000

const MAXIMUM_MANAGEMENT_RESOURCES = 128
const MAXIMUM_MANAGEMENT_RESOURCE_BYTES = 1024 * 1024
const MAXIMUM_MANAGEMENT_TOTAL_BYTES = 8 * 1024 * 1024
const RESOURCE_CONCURRENCY = 4
const PATH_SEGMENT = /^(?!\.{1,2}$)[\w.-]+$/u

const FRAME_CSP = [
  'default-src \'none\'',
  'script-src \'unsafe-inline\' data:',
  'style-src \'unsafe-inline\' data:',
  'img-src data:',
  'font-src data:',
  'media-src data:',
  'connect-src \'none\'',
  'worker-src \'none\'',
  'child-src \'none\'',
  'frame-src \'none\'',
  'object-src \'none\'',
  'manifest-src \'none\'',
  'base-uri \'none\'',
  'form-action \'none\'',
].join('; ')

const JAVASCRIPT_CONTENT_TYPES = new Set([
  'application/ecmascript',
  'application/javascript',
  'text/ecmascript',
  'text/javascript',
])

export interface PluginFrameTheme {
  name: 'light' | 'dark'
  tokens: Record<string, string>
}

export interface AssembledPluginPage {
  srcdoc: string
  revoke: () => void
}

interface LoadedResource {
  descriptor: PluginManagementResource
  body: ArrayBuffer
}

export async function assemblePluginManagementPage(
  view: PluginManagementView,
  page: PluginManagementPage,
  channel: string,
  session: string,
  parentOrigin: string,
  theme: PluginFrameTheme,
  viewportHeight: number,
  signal: AbortSignal,
): Promise<AssembledPluginPage> {
  validateView(view, page)
  const resources = await loadResources(view, signal)
  if (signal.aborted)
    throw new DOMException('Aborted', 'AbortError')

  const resourceUrls = new Map<string, string>()
  const building = new Set<string>()
  const entry = resources.get(page.entry)
  if (!entry || contentType(entry.descriptor.contentType) !== 'text/html')
    throw new Error('插件页面入口不是已注册的 HTML 资源')

  const ensureResourceUrl = (path: string): string => {
    const existing = resourceUrls.get(path)
    if (existing)
      return existing
    const resource = resources.get(path)
    if (!resource)
      throw new Error(`插件页面引用了未注册资源：${path}`)
    const mime = contentType(resource.descriptor.contentType)
    if (mime === 'text/html')
      throw new Error('插件 HTML 资源不能作为子资源加载')
    if (building.has(path))
      throw new Error(`插件 CSS 资源存在循环引用：${path}`)
    building.add(path)
    try {
      const body = mime === 'text/css'
        ? rewriteCss(decodeText(resource.body, path), path, resources, ensureResourceUrl)
        : resource.body
      const url = dataUrl(body, resource.descriptor.contentType)
      resourceUrls.set(path, url)
      return url
    }
    finally {
      building.delete(path)
    }
  }

  for (const [path, resource] of resources) {
    if (contentType(resource.descriptor.contentType) !== 'text/html')
      ensureResourceUrl(path)
  }
  const document = new DOMParser().parseFromString(decodeText(entry.body, page.entry), 'text/html')
  rewriteDocument(document, page.entry, resources, ensureResourceUrl)
  injectFrameBoundary(document, createBridgeScript({
    view,
    page,
    channel,
    session,
    parentOrigin,
    theme,
    viewportHeight,
    resourceUrls: Object.fromEntries(resourceUrls),
  }))
  return {
    srcdoc: `<!doctype html>\n${document.documentElement.outerHTML}`,
    revoke: () => {},
  }
}

export function readPluginFrameTheme(): PluginFrameTheme {
  const root = document.documentElement
  const computed = getComputedStyle(root)
  const tokens: Record<string, string> = {}
  for (const token of THEME_TOKEN_NAMES) {
    const value = safeThemeValue(computed.getPropertyValue(token))
    if (value)
      tokens[token] = value
  }
  const font = safeThemeValue(computed.getPropertyValue('--font-sans')) || safeThemeValue(computed.fontFamily)
  const fontCode = safeThemeValue(computed.getPropertyValue('--font-mono'))
  if (font)
    tokens['--cp-font-family'] = font
  if (fontCode)
    tokens['--cp-font-family-code'] = fontCode
  return {
    name: root.dataset.theme === 'dark' ? 'dark' : 'light',
    tokens,
  }
}

function validateView(view: PluginManagementView, page: PluginManagementPage) {
  if (!Number.isSafeInteger(view.target.revision) || view.target.revision < 1)
    throw new Error('插件页面版本无效，请刷新后重试')
  if (view.resources.length > MAXIMUM_MANAGEMENT_RESOURCES)
    throw new Error('插件页面资源数量超过限制')
  for (const resource of view.resources)
    validateDeclaredPath(resource.path)
  validateDeclaredPath(page.entry)
  if (!view.pages.some(candidate => candidate.id === page.id && candidate.entry === page.entry))
    throw new Error('插件页面已变更，请刷新后重试')
}

async function loadResources(view: PluginManagementView, signal: AbortSignal) {
  const resources = new Map<string, LoadedResource>()
  let cursor = 0
  let total = 0
  const workers = Array.from(
    { length: Math.min(RESOURCE_CONCURRENCY, view.resources.length) },
    async () => {
      while (cursor < view.resources.length) {
        const descriptor = view.resources[cursor++]
        if (!descriptor)
          return
        const response = await getPluginManagementResource(view.target, descriptor.path, { signal, silent: true })
        if (response.status < 200 || response.status >= 300)
          throw new Error(`插件资源返回 HTTP ${response.status}`)
        if (contentType(response.contentType) !== contentType(descriptor.contentType))
          throw new Error(`插件资源类型与注册值不一致：${descriptor.path}`)
        if (response.body.byteLength > MAXIMUM_MANAGEMENT_RESOURCE_BYTES)
          throw new Error(`插件资源超过 1 MiB：${descriptor.path}`)
        total += response.body.byteLength
        if (total > MAXIMUM_MANAGEMENT_TOTAL_BYTES)
          throw new Error('插件页面资源总量超过 8 MiB')
        resources.set(descriptor.path, { descriptor, body: response.body })
      }
    },
  )
  await Promise.all(workers)
  return resources
}

function rewriteDocument(
  document: Document,
  entryPath: string,
  resources: Map<string, LoadedResource>,
  ensureResourceUrl: (path: string) => string,
) {
  const unsupported = document.querySelector('base, iframe, frame, frameset, object, embed, portal, template')
  if (unsupported)
    throw new Error(`插件页面使用了不受支持的 <${unsupported.tagName.toLowerCase()}> 元素`)
  if ([...document.querySelectorAll('meta[http-equiv]')].some(meta => meta.getAttribute('http-equiv')?.toLowerCase() === 'refresh'))
    throw new Error('插件页面不能使用自动跳转')

  for (const script of document.querySelectorAll('script')) {
    if (script.getAttribute('type')?.trim().toLowerCase() === 'module')
      throw new Error('插件页面暂不支持 ES Module 脚本，请使用经典脚本包')
    const source = script.getAttribute('src')
    if (!source)
      continue
    const path = resolveResourceReference(entryPath, source, resources)
    if (!JAVASCRIPT_CONTENT_TYPES.has(contentType(resources.get(path)?.descriptor.contentType ?? '')))
      throw new Error(`插件脚本资源类型无效：${path}`)
    script.setAttribute('src', ensureResourceUrl(path))
  }

  for (const link of document.querySelectorAll('link[href]')) {
    const rel = new Set((link.getAttribute('rel') ?? '').toLowerCase().split(/\s+/u).filter(Boolean))
    if (![...rel].every(value => value === 'stylesheet' || value === 'icon') || rel.size === 0)
      throw new Error('插件页面只支持 stylesheet 和 icon 链接资源')
    const path = resolveResourceReference(entryPath, link.getAttribute('href') ?? '', resources)
    const mime = contentType(resources.get(path)?.descriptor.contentType ?? '')
    if (rel.has('stylesheet') && mime !== 'text/css')
      throw new Error(`插件样式资源类型无效：${path}`)
    if (rel.has('icon') && !mime.startsWith('image/'))
      throw new Error(`插件图标资源类型无效：${path}`)
    link.setAttribute('href', ensureResourceUrl(path))
  }

  const supportedSourceElements = new Set(['AUDIO', 'IMG', 'SCRIPT', 'SOURCE', 'TRACK', 'VIDEO'])
  for (const element of document.querySelectorAll<HTMLElement>('[src]')) {
    if (element.tagName === 'SCRIPT')
      continue
    if (!supportedSourceElements.has(element.tagName))
      throw new Error(`插件页面不支持 <${element.tagName.toLowerCase()}> 的 src 加载`)
    const path = resolveResourceReference(entryPath, element.getAttribute('src') ?? '', resources)
    if (element.tagName === 'IMG' && !contentType(resources.get(path)?.descriptor.contentType ?? '').startsWith('image/'))
      throw new Error(`插件图像资源类型无效：${path}`)
    element.setAttribute('src', ensureResourceUrl(path))
  }

  for (const element of document.querySelectorAll<HTMLElement>('[poster]')) {
    const path = resolveResourceReference(entryPath, element.getAttribute('poster') ?? '', resources)
    if (!contentType(resources.get(path)?.descriptor.contentType ?? '').startsWith('image/'))
      throw new Error(`插件 poster 资源类型无效：${path}`)
    element.setAttribute('poster', ensureResourceUrl(path))
  }

  for (const element of document.querySelectorAll<HTMLElement>('[srcset]')) {
    const rewritten = (element.getAttribute('srcset') ?? '').split(',').map((candidate) => {
      const [reference, ...descriptor] = candidate.trim().split(/\s+/u)
      if (!reference)
        throw new Error('插件 srcset 资源引用为空')
      const path = resolveResourceReference(entryPath, reference, resources)
      return [ensureResourceUrl(path), ...descriptor].join(' ')
    }).join(', ')
    element.setAttribute('srcset', rewritten)
  }

  for (const style of document.querySelectorAll('style'))
    style.textContent = rewriteCss(style.textContent ?? '', entryPath, resources, ensureResourceUrl)
  for (const element of document.querySelectorAll<HTMLElement>('[style]'))
    element.setAttribute('style', rewriteCss(element.getAttribute('style') ?? '', entryPath, resources, ensureResourceUrl, false))

  for (const element of document.querySelectorAll<HTMLElement>('*')) {
    for (const attribute of [...element.attributes]) {
      const name = attribute.name.toLowerCase()
      if (name === 'href' || name === 'xlink:href') {
        if (element.tagName === 'LINK')
          continue
        if (attribute.value.startsWith('#'))
          continue
        throw new Error('插件页面不能导航到外部或未注册地址')
      }
      if (['action', 'formaction', 'ping', 'background'].includes(name) && attribute.value)
        throw new Error(`插件页面不支持 ${name} 地址`)
      if (
        name !== 'style'
        && /\burl\s*\(/iu.test(attribute.value)
        && !/^url\(\s*#[\w.-]+\s*\)$/iu.test(attribute.value.trim())
      ) {
        throw new Error(`插件页面属性不能引用外部资源：${name}`)
      }
    }
  }
}

function injectFrameBoundary(document: Document, bridgeScript: string) {
  const head = document.head || document.documentElement.insertBefore(document.createElement('head'), document.body)
  const policy = document.createElement('meta')
  policy.setAttribute('http-equiv', 'Content-Security-Policy')
  policy.setAttribute('content', FRAME_CSP)
  const referrer = document.createElement('meta')
  referrer.setAttribute('name', 'referrer')
  referrer.setAttribute('content', 'no-referrer')
  const bridge = document.createElement('script')
  bridge.textContent = bridgeScript
  const layout = document.createElement('style')
  // 宿主统一提供最小高度与整页滚动；flow-root 包含子元素外边距与浮动。
  layout.textContent = `
    html { overflow: hidden; }
    body { margin: 0; display: flow-root; box-sizing: border-box; min-height: var(--cp-plugin-viewport-height, 0px); }
  `
  head.append(layout)
  head.prepend(bridge)
  head.prepend(referrer)
  head.prepend(policy)
}

function rewriteCss(
  source: string,
  ownerPath: string,
  resources: Map<string, LoadedResource>,
  ensureResourceUrl: (path: string) => string,
  allowImports = true,
) {
  // 按 CSS 语法区分选择器转义与资源地址，避免拒绝 Tailwind 类名或漏过转义 URL。
  const tree = parse(source, {
    context: allowImports ? 'stylesheet' : 'declarationList',
    parseRulePrelude: false,
    parseCustomProperty: true,
    onParseError(_error, fallback) {
      // 条件表达式会先尝试不同语法；只有落入原始文本兜底才算解析失败。
      if (fallback)
        throw new Error(`插件 CSS 语法不受支持：${ownerPath}`)
    },
  })
  const imported = new WeakSet<object>()
  walk(tree, function (node) {
    // 选择器不加载资源，原样保留其转义；声明值则必须完整解析后再检查地址。
    if (node.type === 'Raw' && node !== this.rule?.prelude)
      throw new Error(`插件 CSS 包含无法解析的内容：${ownerPath}`)
    if ((node.type === 'Atrule' || node.type === 'Function') && node.name.includes('\\'))
      throw new Error(`插件 CSS 规则或函数名不能使用转义：${ownerPath}`)
    if (node.type === 'Atrule' && node.name.toLowerCase() === 'import') {
      if (!allowImports)
        throw new Error('内联 style 属性不能导入样式表')
      const reference = node.prelude?.type === 'AtrulePrelude' ? node.prelude.children.first : null
      if (node.block || !reference || (reference.type !== 'String' && reference.type !== 'Url'))
        throw new Error(`插件 @import 语法不受支持：${ownerPath}`)
      const path = resolveResourceReference(ownerPath, reference.value, resources)
      if (contentType(resources.get(path)?.descriptor.contentType ?? '') !== 'text/css')
        throw new Error(`插件 @import 目标不是 CSS：${path}`)
      reference.value = ensureResourceUrl(path)
      imported.add(reference)
    }
    if (node.type === 'Url' && !imported.has(node)) {
      if (node.value.startsWith('#'))
        return
      const path = resolveResourceReference(ownerPath, node.value, resources)
      node.value = ensureResourceUrl(path)
    }
  })
  return generate(tree)
}

function resolveResourceReference(
  ownerPath: string,
  rawReference: string,
  resources: Map<string, LoadedResource>,
) {
  const reference = rawReference.trim()
  if (
    !reference
    || reference.startsWith('/')
    || reference.startsWith('//')
    || reference.includes('\\')
    || reference.includes('%')
    || hasControlCharacter(reference)
    || /^[a-z][a-z\d+.-]*:/iu.test(reference)
  ) {
    throw new Error(`插件资源地址不受支持：${rawReference}`)
  }
  const queryIndex = reference.indexOf('?')
  if (queryIndex >= 0)
    throw new Error(`插件资源地址不支持查询参数：${rawReference}`)
  if (reference.includes('#'))
    throw new Error(`插件资源地址不支持片段标识：${rawReference}`)
  const pathReference = reference
  const base = ownerPath.split('/').slice(0, -1)
  for (const segment of pathReference.split('/')) {
    if (!segment || segment === '.')
      continue
    if (segment === '..') {
      if (base.length === 0)
        throw new Error(`插件资源地址越过制品边界：${rawReference}`)
      base.pop()
      continue
    }
    if (!PATH_SEGMENT.test(segment))
      throw new Error(`插件资源路径片段无效：${rawReference}`)
    base.push(segment)
  }
  const path = base.join('/')
  if (!resources.has(path))
    throw new Error(`插件页面引用了未注册资源：${path}`)
  return path
}

function validateDeclaredPath(path: string) {
  if (!path || path.length > 512 || path.split('/').some(segment => !PATH_SEGMENT.test(segment)))
    throw new Error(`插件注册了无效资源路径：${path}`)
}

function contentType(value: string) {
  return value.split(';', 1)[0]?.trim().toLowerCase() ?? ''
}

function decodeText(body: ArrayBuffer, path: string) {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(body))
  }
  catch {
    throw new Error(`插件文本资源不是有效 UTF-8：${path}`)
  }
}

function dataUrl(body: ArrayBuffer | string, mime: string) {
  const bytes = typeof body === 'string'
    ? new TextEncoder().encode(body)
    : new Uint8Array(body)
  let binary = ''
  const chunkSize = 32 * 1024
  for (let offset = 0; offset < bytes.byteLength; offset += chunkSize)
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize))
  return `data:${mime};base64,${btoa(binary)}`
}

function safeThemeValue(value: string) {
  const normalized = value.trim()
  if (!normalized || normalized.length > 512 || /url\s*\(|[<>]/iu.test(normalized) || hasControlCharacter(normalized))
    return ''
  return normalized
}

function hasControlCharacter(value: string) {
  return [...value].some((character) => {
    const code = character.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7F
  })
}

function createBridgeScript(input: {
  view: PluginManagementView
  page: PluginManagementPage
  channel: string
  session: string
  parentOrigin: string
  theme: PluginFrameTheme
  viewportHeight: number
  resourceUrls: Record<string, string>
}) {
  const initial = JSON.stringify({
    target: PLUGIN_MANAGEMENT_BRIDGE,
    version: PLUGIN_MANAGEMENT_BRIDGE_VERSION,
    channel: input.channel,
    session: input.session,
    parentOrigin: input.parentOrigin,
    plugin: { name: input.view.name },
    page: { id: input.page.id, title: input.page.title },
    theme: input.theme,
    viewportHeight: input.viewportHeight,
    resourceUrls: input.resourceUrls,
  }).replace(/</gu, '\\u003c')
  return `(() => {
    'use strict';
    const initial = ${initial};
    const pending = new Map();
    const modelOperations = new Map();
    const safeModelHeaders = new Set([
      'content-type', 'openai-processing-ms', 'openai-request-id', 'request-id', 'retry-after',
      'x-client-request-id', 'x-gateway-request-id', 'x-oai-request-id', 'x-openai-request-id', 'x-processing-ms', 'x-request-id',
      'x-ratelimit-limit-requests', 'x-ratelimit-limit-tokens', 'x-ratelimit-remaining-requests',
      'x-ratelimit-remaining-tokens', 'x-ratelimit-reset-requests', 'x-ratelimit-reset-tokens',
    ]);
    let sequence = 0;
    let resizeObserver;
    let resizeFrame = 0;
    let previousHeight = 0;
    let disposed = false;
    let currentTheme = initial.theme.name;
    const maximumBodyBytes = ${MAXIMUM_MANAGEMENT_BODY_BYTES};
    const maximumQueryBytes = ${MAXIMUM_MANAGEMENT_QUERY_BYTES};
    const maximumInFlight = ${MAXIMUM_MANAGEMENT_IN_FLIGHT};
    const maximumModelBodyBytes = ${MAXIMUM_MODEL_BODY_BYTES};
    const maximumModelChunkBytes = ${MAXIMUM_MODEL_CHUNK_BYTES};
    const maximumModelInFlight = ${MAXIMUM_MODEL_IN_FLIGHT};
    const modelRequestDeadlineMs = ${MODEL_REQUEST_DEADLINE_MS};

    function applyLayout(height) {
      if (Number.isSafeInteger(height) && height >= 0) {
        document.documentElement.style.setProperty('--cp-plugin-viewport-height', height + 'px');
      }
    }

    function applyTheme(theme) {
      if (!theme || (theme.name !== 'light' && theme.name !== 'dark') || !theme.tokens || typeof theme.tokens !== 'object') return;
      const root = document.documentElement;
      root.dataset.theme = theme.name;
      root.style.colorScheme = theme.name;
      for (const [name, value] of Object.entries(theme.tokens)) {
        if (typeof name === 'string' && name.startsWith('--cp-') && typeof value === 'string') root.style.setProperty(name, value);
      }
      currentTheme = theme.name;
      window.dispatchEvent(new CustomEvent('codex-proxy-themechange', { detail: { theme: currentTheme } }));
    }

    async function bodyBytes(body) {
      if (body == null) return new ArrayBuffer(0);
      if (typeof body === 'string') return new TextEncoder().encode(body).buffer;
      if (body instanceof ArrayBuffer) return body.slice(0);
      if (ArrayBuffer.isView(body)) return body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength);
      throw new TypeError('请求正文只支持 string、ArrayBuffer 或 TypedArray');
    }

    function nextRequestId() {
      sequence += 1;
      if (!Number.isSafeInteger(sequence)) throw new Error('插件页面请求序号已耗尽');
      return sequence;
    }

    function post(type, id, payload, transfer = []) {
      parent.postMessage({
        target: initial.target,
        version: initial.version,
        channel: initial.channel,
        session: initial.session,
        type,
        id,
        payload,
      }, initial.parentOrigin, transfer);
    }

    function invoke(kind, payload, transfer = []) {
      if (pending.size >= maximumInFlight) return Promise.reject(new Error('插件管理请求已达并发上限'));
      const id = nextRequestId();
      return new Promise((resolve, reject) => {
        const timer = window.setTimeout(() => {
          pending.delete(id);
          reject(new Error('插件管理请求超时'));
        }, kind === 'request' ? ${PLUGIN_MANAGEMENT_TIMEOUT_MS} : ${API_TIMEOUT_MS});
        pending.set(id, { resolve, reject, timer });
        post(kind, id, payload, transfer);
      });
    }

    function reportContentHeight() {
      // 不读 documentElement.scrollHeight，避免当前 iframe 高度阻止内容收缩。
      const height = Math.max(1, Math.ceil(document.body.getBoundingClientRect().height));
      if (height === previousHeight) return;
      previousHeight = height;
      post('resize', nextRequestId(), { height });
    }

    async function observeContentHeight() {
      // load 后先触发布局，让实际使用的字体进入加载集合，再等待字体与布局就绪。
      document.body.getBoundingClientRect();
      await document.fonts.ready;
      if (disposed) return;
      // 首次上报不依赖渲染回调，后台或视口外的 iframe 也能完成初始化。
      reportContentHeight();
      resizeObserver = new ResizeObserver(() => {
        if (resizeFrame) return;
        resizeFrame = requestAnimationFrame(() => {
          resizeFrame = 0;
          reportContentHeight();
        });
      });
      resizeObserver.observe(document.body, { box: 'border-box' });
    }

    window.addEventListener('load', observeContentHeight, { once: true });

    async function request(input) {
      if (!input || typeof input !== 'object') throw new TypeError('插件管理请求无效');
      const method = String(input.method || '').toUpperCase();
      const path = String(input.path || '');
      const query = input.query == null ? '' : String(input.query);
      const contentType = input.contentType == null ? undefined : String(input.contentType);
      if (new TextEncoder().encode(query).byteLength > maximumQueryBytes || query.includes('#') || query.startsWith('?')) throw new TypeError('插件管理查询参数无效');
      const body = await bodyBytes(input.body);
      if (body.byteLength > maximumBodyBytes) throw new TypeError('插件管理请求正文超过 1 MiB');
      return invoke('request', { method, path, query, contentType, body }, [body]);
    }

    function callbackTicket(input) {
      if (!input || typeof input !== 'object') return Promise.reject(new TypeError('插件回调票据请求无效'));
      return invoke('callback-ticket', { path: String(input.path || ''), ttlSeconds: Number(input.ttlSeconds) });
    }

    function abortError(reason) {
      return reason instanceof Error ? reason : new DOMException('模型请求已取消', 'AbortError');
    }

    function clearModelOperation(operation) {
      if (modelOperations.get(operation.id) !== operation) return;
      modelOperations.delete(operation.id);
      clearTimeout(operation.timer);
      if (operation.signal) operation.signal.removeEventListener('abort', operation.abort);
    }

    function settleModelPull(operation, error) {
      const resolve = operation.pullResolve;
      const reject = operation.pullReject;
      operation.pullResolve = undefined;
      operation.pullReject = undefined;
      operation.pullPromise = undefined;
      operation.pullId = undefined;
      if (error) reject?.(error);
      else resolve?.();
    }

    function failModelOperation(operation, error, notifyParent) {
      if (modelOperations.get(operation.id) !== operation) return;
      clearModelOperation(operation);
      settleModelPull(operation, error);
      if (operation.controller) {
        try { operation.controller.error(error); } catch {}
      } else {
        operation.reject(error);
      }
      if (notifyParent) post('models-cancel', operation.id);
    }

    function finishModelOperation(operation) {
      if (modelOperations.get(operation.id) !== operation) return;
      clearModelOperation(operation);
      settleModelPull(operation);
    }

    function validateResponseHeaders(value) {
      if (!Array.isArray(value) || value.length > 32) throw new Error('模型响应头无效');
      const headers = [];
      let total = 0;
      for (const entry of value) {
        if (!Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== 'string' || typeof entry[1] !== 'string') throw new Error('模型响应头无效');
        const name = entry[0].toLowerCase();
        const headerValue = entry[1];
        total += name.length + headerValue.length;
        if (!safeModelHeaders.has(name) || headerValue.length > 8192 || total > 32768 || /[\\u0000-\\u0008\\u000A-\\u001F\\u007F]/u.test(headerValue)) throw new Error('模型响应头无效');
        headers.push([name, headerValue]);
      }
      return headers;
    }

    function handleModelMessage(message) {
      const operation = modelOperations.get(message.id);
      if (!operation) return;
      if (message.type === 'models-error') {
        const text = typeof message.error === 'string' && message.error ? message.error : '模型请求失败';
        failModelOperation(operation, new Error(text), false);
        return;
      }
      if (message.type === 'models-response') {
        if (operation.controller || operation.responseStarted || !message.payload || typeof message.payload !== 'object') {
          failModelOperation(operation, new Error('模型响应协议无效'), true);
          return;
        }
        try {
          const status = message.payload.status;
          const statusText = message.payload.statusText == null ? '' : message.payload.statusText;
          const hasBody = message.payload.hasBody;
          if (!Number.isSafeInteger(status) || status < 200 || status > 599 || typeof statusText !== 'string' || statusText.length > 256 || /[\\u0000-\\u001F\\u007F]/u.test(statusText) || typeof hasBody !== 'boolean') throw new Error('模型响应状态无效');
          const headers = validateResponseHeaders(message.payload.headers);
          operation.responseStarted = true;
          if (!hasBody) {
            finishModelOperation(operation);
            operation.resolve(new Response(null, { status, statusText, headers }));
            return;
          }
          const stream = new ReadableStream({
            start(controller) {
              operation.controller = controller;
            },
            pull() {
              if (modelOperations.get(operation.id) !== operation) return undefined;
              if (operation.pullPromise) return operation.pullPromise;
              operation.pullSequence += 1;
              operation.pullId = operation.pullSequence;
              operation.pullPromise = new Promise((resolve, reject) => {
                operation.pullResolve = resolve;
                operation.pullReject = reject;
                post('models-pull', operation.id, { pullId: operation.pullId });
              });
              return operation.pullPromise;
            },
            cancel() {
              if (modelOperations.get(operation.id) === operation) {
                clearModelOperation(operation);
                settleModelPull(operation);
                post('models-cancel', operation.id);
              }
            },
          }, { highWaterMark: 0 });
          operation.resolve(new Response(stream, { status, statusText, headers }));
        } catch (error) {
          failModelOperation(operation, error instanceof Error ? error : new Error('模型响应协议无效'), true);
        }
        return;
      }
      if (!operation.controller || !operation.responseStarted) {
        failModelOperation(operation, new Error('模型响应协议无效'), true);
        return;
      }
      if (message.type === 'models-chunk') {
        const pullId = message.payload?.pullId;
        const body = message.payload?.body;
        if (!operation.pullPromise || !Number.isSafeInteger(pullId) || pullId !== operation.pullId || !(body instanceof ArrayBuffer) || body.byteLength === 0 || body.byteLength > maximumModelChunkBytes) {
          failModelOperation(operation, new Error('模型响应分块无效'), true);
          return;
        }
        try {
          operation.controller.enqueue(new Uint8Array(body));
          settleModelPull(operation);
        } catch (error) {
          failModelOperation(operation, error instanceof Error ? error : new Error('模型响应读取失败'), true);
        }
        return;
      }
      if (message.type === 'models-complete') {
        const pullId = message.payload?.pullId;
        if (!operation.pullPromise || !Number.isSafeInteger(pullId) || pullId !== operation.pullId) {
          failModelOperation(operation, new Error('模型响应结束标记无效'), true);
          return;
        }
        try { operation.controller.close(); } catch {}
        finishModelOperation(operation);
      }
    }

    function modelResponses(input) {
      if (!input || typeof input !== 'object' || !input.body || typeof input.body !== 'object' || Array.isArray(input.body)) return Promise.reject(new TypeError('Responses 请求正文必须是 JSON 对象'));
      if (typeof input.clientKeyId !== 'string' || !input.clientKeyId || input.clientKeyId !== input.clientKeyId.trim() || input.clientKeyId.length > 128 || /[\\u0000-\\u001F\\u007F]/u.test(input.clientKeyId)) return Promise.reject(new TypeError('clientKeyId 无效'));
      if (input.signal !== undefined && !(input.signal instanceof AbortSignal)) return Promise.reject(new TypeError('signal 必须是 AbortSignal'));
      if (input.signal?.aborted) return Promise.reject(abortError(input.signal.reason));
      if (modelOperations.size >= maximumModelInFlight) return Promise.reject(new Error('模型请求已达并发上限'));
      let body;
      try {
        body = JSON.stringify(input.body);
      } catch {
        return Promise.reject(new TypeError('Responses 请求正文必须可序列化为 JSON'));
      }
      if (typeof body !== 'string' || new TextEncoder().encode(body).byteLength > maximumModelBodyBytes) return Promise.reject(new TypeError('Responses 请求正文超过 8 MiB'));
      const id = nextRequestId();
      return new Promise((resolve, reject) => {
        const operation = {
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
        };
        operation.abort = () => failModelOperation(operation, abortError(input.signal?.reason), true);
        operation.timer = window.setTimeout(() => failModelOperation(operation, new Error('模型请求超过 10 分钟'), true), modelRequestDeadlineMs);
        modelOperations.set(id, operation);
        input.signal?.addEventListener('abort', operation.abort, { once: true });
        post('models-request', id, { clientKeyId: input.clientKeyId, body });
      });
    }

    document.addEventListener('click', (event) => {
      if (event.defaultPrevented || !(event.target instanceof Element)) return;
      const submitter = event.target.closest('button, input');
      if (!(submitter instanceof HTMLButtonElement || submitter instanceof HTMLInputElement)) return;
      const type = submitter.type.toLowerCase();
      if (type !== 'submit' && type !== 'image') return;
      const form = submitter.form;
      if (!form) return;
      event.preventDefault();
      queueMicrotask(() => {
        if (!form.isConnected || !submitter.isConnected) return;
        if (!submitter.formNoValidate && !form.reportValidity()) return;
        form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true, submitter }));
      });
    });

    window.addEventListener('message', (event) => {
      if (event.source !== parent || event.origin !== initial.parentOrigin) return;
      const message = event.data;
      if (!message || message.target !== initial.target || message.version !== initial.version || message.channel !== initial.channel || message.session !== initial.session) return;
      if (message.type === 'theme') {
        applyTheme(message.payload);
        return;
      }
      if (message.type === 'layout') {
        applyLayout(message.payload?.viewportHeight);
        return;
      }
      if (!Number.isSafeInteger(message.id) || message.id < 1) return;
      if (message.type === 'models-response' || message.type === 'models-chunk' || message.type === 'models-complete' || message.type === 'models-error') {
        handleModelMessage(message);
        return;
      }
      if (message.type !== 'result') return;
      const operation = pending.get(message.id);
      if (!operation) return;
      pending.delete(message.id);
      clearTimeout(operation.timer);
      if (message.ok) operation.resolve(message.payload);
      else operation.reject(new Error(typeof message.error === 'string' ? message.error : '插件管理请求失败'));
    });

    window.addEventListener('pagehide', () => {
      disposed = true;
      resizeObserver?.disconnect();
      cancelAnimationFrame(resizeFrame);
      for (const operation of [...modelOperations.values()]) {
        clearModelOperation(operation);
        settleModelPull(operation);
        post('models-cancel', operation.id);
      }
    }, { once: true });

    const api = {
      version: initial.version,
      plugin: Object.freeze(initial.plugin),
      page: Object.freeze(initial.page),
      get theme() { return currentTheme; },
      request,
      callbackTicket,
      models: Object.freeze({ responses: modelResponses }),
      resourceUrl(path) {
        const key = String(path);
        if (!Object.prototype.hasOwnProperty.call(initial.resourceUrls, key)) throw new Error('资源未注册或不可作为页面子资源');
        const value = initial.resourceUrls[key];
        return value;
      },
    };
    Object.defineProperty(window, 'codexProxyPlugin', { value: Object.freeze(api), configurable: false, writable: false });
    applyTheme(initial.theme);
    applyLayout(initial.viewportHeight);
  })();`
}
