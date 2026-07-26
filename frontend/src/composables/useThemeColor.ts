import { storeToRefs } from 'pinia'

import { useUiStore } from '@/stores/modules/ui'
import { readCssVariable } from '@/utils/css'

// 主题感知的 CSS 变量读取：themeRevision 作为响应式依赖，主题切换后
// 引用该函数的 computed 会重算并读到新变量值。
export function useThemeColor() {
  const { themeRevision } = storeToRefs(useUiStore())
  return (name: string, fallback: string) => {
    void themeRevision.value
    return readCssVariable(name, fallback)
  }
}
