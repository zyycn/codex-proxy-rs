export type Locale = 'en' | 'zh-Hans' | 'zh-Hant'

export const localeOptions = [
  { label: '简体中文', value: 'zh-Hans' },
  { label: '繁體中文', value: 'zh-Hant' },
  { label: 'English', value: 'en' },
] satisfies { label: string, value: Locale }[]

const storageKey = 'codex-proxy-rs-locale'

export function isLocale(value: unknown): value is Locale {
  return localeOptions.some(option => option.value === value)
}

export function normalizeLocale(language: string): Locale {
  try {
    const { language: base, script, region } = new Intl.Locale(language)
    if (base === 'zh') {
      if (script === 'Hant')
        return 'zh-Hant'
      if (script === 'Hans')
        return 'zh-Hans'
      return region && ['TW', 'HK', 'MO'].includes(region)
        ? 'zh-Hant'
        : 'zh-Hans'
    }
  }
  catch {
    // 无效的浏览器语言使用应用默认语言
  }
  return 'en'
}

export function readInitialLocale(): Locale {
  try {
    const saved = localStorage.getItem(storageKey)
    if (isLocale(saved))
      return saved
  }
  catch {
    // 存储不可用时继续使用浏览器语言
  }
  return normalizeLocale(navigator.language)
}

export function applyLocale(value: string): void {
  document.documentElement.lang = value
  try {
    localStorage.setItem(storageKey, value)
  }
  catch {
    // 存储不可用不影响当前会话的语言切换
  }
}
