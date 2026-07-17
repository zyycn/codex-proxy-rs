// @env browser

export function readCssVariable(name: string, fallback: string) {
  if (typeof document === 'undefined')
    return fallback
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}
