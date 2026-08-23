import type { InjectionKey, Ref } from 'vue'

import { storeToRefs } from 'pinia'
import { inject, provide } from 'vue'

import { useThemeStore } from '@/stores/modules/theme'

export type ThemeColorTokens = Readonly<Record<string, string | number | undefined>>

const themeColorTokensKey: InjectionKey<Readonly<Ref<ThemeColorTokens>>> = Symbol('theme-color-tokens')

export function provideThemeColorTokens(tokens: Readonly<Ref<ThemeColorTokens>>) {
  provide(themeColorTokensKey, tokens)
}

// 主题感知的 CSS 变量读取：themeRevision 作为响应式依赖，主题切换后
// 引用该函数的 computed 会重算并读到新变量值；影子预览优先读取其局部 Token。
export function useThemeColor() {
  const scopedTokens = inject(themeColorTokensKey, null)
  const { themeRevision } = storeToRefs(useThemeStore())
  return (name: string, fallback: string) => {
    if (scopedTokens) {
      const scopedValue = scopedTokens.value[name]
      if (typeof scopedValue === 'string' && scopedValue.trim())
        return scopedValue.trim()
      if (typeof scopedValue === 'number')
        return String(scopedValue)
      return fallback
    }

    void themeRevision.value
    return readCssVariable(name, fallback)
  }
}

function readCssVariable(name: string, fallback: string) {
  if (typeof document === 'undefined')
    return fallback
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}
