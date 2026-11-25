import type { UiLocale } from '@codex-proxy/ui'
import type { Locale } from './locale'
import { computed } from 'vue'
import { createI18n } from 'vue-i18n'
import { readInitialLocale } from './locale'
import { localeLoaders, uiMessages } from './messages'

export { applyLocale, localeOptions, readInitialLocale } from './locale'
export type { Locale } from './locale'

export const i18n = createI18n({
  legacy: false,
  locale: readInitialLocale(),
  fallbackLocale: false,
  missing: () => '',
  messages: {},
})

// 非组件入口与 Vue 组件共享同一 Composer，调用时读取当前语言
export const t: typeof i18n.global.t = i18n.global.t
export const { locale } = i18n.global
// 挂载与语言切换均等待资源就绪，UI 组件读取同一份已加载翻译
export const uiLocale = computed(() => uiMessages[locale.value as Locale] as UiLocale)

const pendingLoads = new Map<Locale, Promise<void>>()
let selection = 0

function loadLocale(value: Locale): Promise<void> {
  if (uiMessages[value])
    return Promise.resolve()

  const pending = pendingLoads.get(value)
  if (pending)
    return pending

  const loading = localeLoaders[value]().then((messages) => {
    i18n.global.setLocaleMessage(value, messages.default)
    uiMessages[value] = messages.uiLocale
  }).finally(() => {
    pendingLoads.delete(value)
  })
  pendingLoads.set(value, loading)
  return loading
}

export async function setLocale(value: Locale): Promise<boolean> {
  const currentSelection = ++selection
  try {
    await loadLocale(value)
  }
  catch (error) {
    if (currentSelection !== selection)
      return false
    throw error
  }
  // 较早请求的语言包可以缓存，但不能覆盖用户之后的选择
  if (currentSelection !== selection)
    return false

  locale.value = value
  return true
}
