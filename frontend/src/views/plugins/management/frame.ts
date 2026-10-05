import type { PluginBridgeInitial, PluginFrameTheme } from './bridge-contract'
import type {
  PluginManagementPage,
  PluginManagementResource,
  PluginManagementView,
} from '@/api'
import { THEME_TOKEN_NAMES } from '@codex-proxy/ui/theme'
import { generate, parse, walk } from 'css-tree'
import bridgeSource from 'virtual:plugin-bridge-source'
import { getPluginManagementResource, PLUGIN_MANAGEMENT_TIMEOUT_MS } from '@/api'

import { API_TIMEOUT_MS } from '@/api/constants'
import { hasControlCharacter, PLUGIN_MANAGEMENT_BRIDGE, PLUGIN_MANAGEMENT_BRIDGE_VERSION } from './bridge-contract'

export * from './bridge-contract'

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

interface AssembledPluginPage {
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
    managementTimeoutMs: PLUGIN_MANAGEMENT_TIMEOUT_MS,
    apiTimeoutMs: API_TIMEOUT_MS,
  } satisfies PluginBridgeInitial).replace(/</gu, '\\u003c')
  return `${bridgeSource};CodexProxyPluginBridge(${initial});`
}
