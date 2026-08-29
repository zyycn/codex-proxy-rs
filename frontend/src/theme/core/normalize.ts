import type {
  ThemeColorId,
  ThemeComponentOverrides,
  ThemeCustomization,
  ThemeMode,
  ThemeSeedOverrides,
  ThemeTokenName,
  ThemeTokens,
} from '../types'
import { normalizeHexColor } from '../../utils/color'
import {
  EDITABLE_COLOR_TOKEN_NAMES,
  EDITABLE_SHADOW_TOKEN_NAMES,
  THEME_COLOR_PRESETS,
} from './constants'

export function normalizeThemeCustomization(value: unknown): ThemeCustomization {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as { seed?: unknown, component?: unknown, tokenOverrides?: unknown }
  const seed = normalizeThemeSeedOverrides(source.seed)
  const component = normalizeThemeComponentOverrides(source.component)
  const tokenOverrides = normalizeThemeTokenOverrides(source.tokenOverrides)
  return {
    ...(Object.keys(seed).length > 0 ? { seed } : {}),
    ...(Object.keys(component).length > 0 ? { component } : {}),
    ...(Object.keys(tokenOverrides).length > 0 ? { tokenOverrides } : {}),
  }
}

export function isThemeMode(value: unknown): value is ThemeMode {
  return value === 'system' || value === 'light' || value === 'dark'
}

export function isThemeColorId(value: unknown): value is ThemeColorId {
  return value === 'custom' || THEME_COLOR_PRESETS.some(preset => preset.id === value)
}

function normalizeThemeSeedOverrides(value: unknown): ThemeSeedOverrides {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as Record<string, unknown>
  const colors = [
    'colorSuccess',
    'colorWarning',
    'colorError',
    'colorInfo',
    'colorLink',
    'colorTextBase',
    'colorBgBase',
  ] as const
  const result: ThemeSeedOverrides = {}

  for (const key of colors) {
    const normalized = normalizeHexColor(source[key])
    if (normalized)
      result[key] = normalized
  }

  assignBoundedNumber(result, source, 'fontSize', 11, 17)
  assignBoundedNumber(result, source, 'sizeUnit', 3, 6)
  assignBoundedNumber(result, source, 'sizeStep', 2, 8)
  assignBoundedNumber(result, source, 'controlHeight', 28, 52)
  assignBoundedNumber(result, source, 'borderRadius', 0, 20)
  assignBoundedNumber(result, source, 'shadowStrength', 0, 140)
  return result
}

function normalizeThemeComponentOverrides(value: unknown): ThemeComponentOverrides {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as Record<string, unknown>
  const result: ThemeComponentOverrides = {}
  assignBoundedNumber(result, source, 'tableRowHeight', 44, 84)
  assignBoundedNumber(result, source, 'cardBorderRadius', 0, 32)
  return result
}

function assignBoundedNumber<Target extends object, Key extends keyof Target>(
  target: Target,
  source: Record<string, unknown>,
  key: Key,
  min: number,
  max: number,
): void {
  const value = source[key as string]
  if (typeof value === 'number' && Number.isFinite(value))
    target[key] = Math.round(Math.min(max, Math.max(min, value))) as Target[Key]
}

function normalizeThemeTokenOverrides(value: unknown): Partial<ThemeTokens> {
  if (!value || typeof value !== 'object')
    return {}

  const result: Partial<ThemeTokens> = {}
  for (const [rawName, rawValue] of Object.entries(value)) {
    const name = rawName as ThemeTokenName
    if (typeof rawValue !== 'string')
      continue

    if (EDITABLE_COLOR_TOKEN_NAMES.has(name)) {
      const normalized = normalizeHexColor(rawValue)
      if (normalized)
        result[name] = normalized
      continue
    }

    if (EDITABLE_SHADOW_TOKEN_NAMES.has(name) && isSafeShadowValue(rawValue))
      result[name] = rawValue.trim()
  }
  return result
}

function isSafeShadowValue(value: string): boolean {
  const normalized = value.trim()
  return normalized.length > 0
    && normalized.length <= 240
    && !/[;{}]/.test(normalized)
    && !/url\s*\(/i.test(normalized)
}
