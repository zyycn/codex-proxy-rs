// @env browser

import type { ResolvedTheme } from './index'

const RUNTIME_THEME_STYLE_ID = 'cp-runtime-theme'

export function applyResolvedTheme(root: HTMLElement, theme: ResolvedTheme): void {
  root.dataset.theme = theme.name
  root.dataset.themeColor = theme.colorId

  const style = ensureRuntimeThemeStyle(root.ownerDocument)
  style.dataset.theme = theme.name
  style.dataset.themeColor = theme.colorId
  style.textContent = serializeTheme(theme)
}

function ensureRuntimeThemeStyle(document: Document): HTMLStyleElement {
  const existing = document.getElementById(RUNTIME_THEME_STYLE_ID)
  if (existing?.tagName === 'STYLE')
    return existing as HTMLStyleElement

  existing?.remove()
  const style = document.createElement('style')
  style.id = RUNTIME_THEME_STYLE_ID
  style.setAttribute('data-cp-runtime-theme', '')
  document.head.append(style)
  return style
}

function serializeTheme(theme: ResolvedTheme): string {
  const declarations = Object.entries(theme.tokens)
    .map(([name, value]) => `  ${name}: ${value};`)
    .join('\n')

  return `:root[data-theme='${theme.name}'][data-theme-color='${theme.colorId}'] {\n  color-scheme: ${theme.name};\n${declarations}\n}\n`
}
