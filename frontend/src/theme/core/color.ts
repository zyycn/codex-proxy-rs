import type {
  ColorToneOptions,
  RgbColor,
  ThemeName,
} from '../types'
import { generate } from '@ant-design/colors'
import { FastColor } from '@ant-design/fast-color'
import { normalizeHexColor } from '../../utils/color'
import {
  BLACK,
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_DIMENSIONS,
  WHITE,
} from './constants'

const MAX_RGB_DISTANCE = Math.sqrt(3) * 255

export function generateColorPalette(
  seed: string,
  theme: ThemeName,
  containerBg?: string,
): string[] {
  return generate(seed, theme === 'dark'
    ? { theme: 'dark', ...(containerBg ? { backgroundColor: containerBg } : {}) }
    : undefined)
    .map(color => normalizeHexColor(color) ?? seed)
}

/** 同一个文字角色可能出现在多个表面或交互状态，按最弱的一组对比度校正。 */
export function ensureContrast(color: string, background: string | readonly string[], target: number): string {
  const backgrounds = typeof background === 'string' ? [background] : background
  const minimumContrast = (candidate: string): number =>
    Math.min(...backgrounds.map(value => contrastRatio(candidate, value)))
  if (minimumContrast(color) >= target)
    return normalizeHexColor(color) ?? color

  let bestColor = color
  let bestRatio = minimumContrast(color)

  for (let step = 1; step <= 100; step += 1) {
    const amount = step / 100
    for (const direction of [BLACK, WHITE]) {
      const candidate = mix(color, direction, amount)
      const ratio = minimumContrast(candidate)
      if (ratio > bestRatio) {
        bestColor = candidate
        bestRatio = ratio
      }
      if (ratio >= target)
        return candidate
    }
  }

  return bestColor
}

export function ensureLightness(color: string, minimum: number): string {
  const normalized = normalizeHexColor(color) ?? DEFAULT_CUSTOM_THEME_COLOR
  const hsl = new FastColor(normalized).toHsl()
  const lightness = Math.min(1, Math.max(0, minimum))
  if (hsl.l >= lightness)
    return normalized

  return new FastColor({ ...hsl, l: lightness })
    .toHexString()
    .toUpperCase()
}

export function mix(first: string, second: string, secondWeight: number): string {
  const from = hexToRgb(first)
  const to = hexToRgb(second)
  const weight = Math.min(1, Math.max(0, secondWeight))
  return rgbToHex({
    red: from.red + (to.red - from.red) * weight,
    green: from.green + (to.green - from.green) * weight,
    blue: from.blue + (to.blue - from.blue) * weight,
  })
}

/** RGB 空间的归一化颜色距离，用于在基准主题与带色温主题间连续过渡。 */
export function relativeColorDistance(first: string, second: string): number {
  const from = hexToRgb(first)
  const to = hexToRgb(second)
  return Math.hypot(
    from.red - to.red,
    from.green - to.green,
    from.blue - to.blue,
  ) / MAX_RGB_DISTANCE
}

/** 以背景明度和前景色相生成颜色层级，避免 RGB 混合过早丢失色相。 */
export function mixColorTone(
  background: string,
  foreground: string,
  foregroundWeight: number,
  options: ColorToneOptions = {},
): string {
  const backgroundHsl = new FastColor(background).toHsl()
  const foregroundHsl = new FastColor(foreground).toHsl()
  const weight = Math.min(1, Math.max(0, foregroundWeight))
  const hueWeight = Math.min(1, Math.max(0, options.hueWeight ?? 1))
  const hue = backgroundHsl.s === 0
    ? foregroundHsl.h
    : mixHue(backgroundHsl.h, foregroundHsl.h, hueWeight)
  const saturation = Math.min(
    1,
    foregroundHsl.s * Math.max(0, options.saturationScale ?? 1),
    Math.max(0, options.saturationLimit ?? 1),
  )
  const lightness = backgroundHsl.l
    + (foregroundHsl.l - backgroundHsl.l) * weight

  return new FastColor({ h: hue, s: saturation, l: lightness })
    .toHexString()
    .toUpperCase()
}

function mixHue(first: number, second: number, secondWeight: number): number {
  const difference = ((second - first + 540) % 360) - 180
  return (first + difference * secondWeight + 360) % 360
}

/** 浅色 Surface 限制色度，并随层级加深继续收敛。 */
export function mixSurfaceTone(
  background: string,
  foreground: string,
  foregroundWeight: number,
): string {
  return mixColorTone(background, foreground, foregroundWeight, {
    saturationLimit: Math.max(0, 0.42 - foregroundWeight),
  })
}

export function withAlpha(color: string, alpha: number): string {
  const normalized = normalizeHexColor(color) ?? DEFAULT_CUSTOM_THEME_COLOR
  const alphaHex = Math.round(Math.min(1, Math.max(0, alpha)) * 255)
    .toString(16)
    .padStart(2, '0')
    .toUpperCase()
  return `${normalized}${alphaHex}`
}

export function scaleShadowAlpha(shadow: string, strength: number): string {
  if (strength === DEFAULT_DIMENSIONS.shadowStrength)
    return shadow
  if (strength <= 0)
    return 'none'

  const scale = strength / DEFAULT_DIMENSIONS.shadowStrength
  return shadow.replace(/#([\dA-F]{6})([\dA-F]{2})/gi, (_match, color: string, alpha: string) => {
    const scaledAlpha = Math.min(255, Math.round(Number.parseInt(alpha, 16) * scale))
      .toString(16)
      .padStart(2, '0')
      .toUpperCase()
    return `#${color.toUpperCase()}${scaledAlpha}`
  })
}

function contrastRatio(first: string, second: string): number {
  const firstLuminance = relativeLuminance(hexToRgb(first))
  const secondLuminance = relativeLuminance(hexToRgb(second))
  const lightest = Math.max(firstLuminance, secondLuminance)
  const darkest = Math.min(firstLuminance, secondLuminance)
  return (lightest + 0.05) / (darkest + 0.05)
}

function relativeLuminance(color: RgbColor): number {
  const channels = [color.red, color.green, color.blue].map((channel) => {
    const value = channel / 255
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
  })
  return channels[0]! * 0.2126 + channels[1]! * 0.7152 + channels[2]! * 0.0722
}

function hexToRgb(value: string): RgbColor {
  const normalized = normalizeHexColor(value) ?? DEFAULT_CUSTOM_THEME_COLOR
  return {
    red: Number.parseInt(normalized.slice(1, 3), 16),
    green: Number.parseInt(normalized.slice(3, 5), 16),
    blue: Number.parseInt(normalized.slice(5, 7), 16),
  }
}

function rgbToHex(color: RgbColor): string {
  const value = [color.red, color.green, color.blue]
    .map(channel => Math.round(Math.min(255, Math.max(0, channel))).toString(16).padStart(2, '0'))
    .join('')
  return `#${value}`.toUpperCase()
}
