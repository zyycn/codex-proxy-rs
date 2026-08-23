import { normalizeHexColor, normalizeRgbaHexColor } from '@/utils/color'

const RGBA_COLOR_PATTERN
  = /^rgba\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(0(?:\.\d+)?|1(?:\.0+)?)\s*\)$/i

export interface HsvaColor {
  h: number
  s: number
  v: number
  a: number
}

interface RgbaColor {
  r: number
  g: number
  b: number
  a: number
}

export function normalizePickerHexColor(value: string, allowAlpha: boolean) {
  if (allowAlpha) {
    const rgba = normalizeRgbaHexColor(value)
    if (rgba)
      return rgba
    const rgb = normalizeHexColor(value)
    return rgb ? `${rgb}FF` : null
  }

  const rgb = normalizeHexColor(value)
  if (rgb)
    return rgb
  return normalizeRgbaHexColor(value)?.slice(0, 7) ?? null
}

export function hexToHsva(value: string): HsvaColor | null {
  const normalized = normalizePickerHexColor(value, true)
  if (!normalized)
    return null

  const red = Number.parseInt(normalized.slice(1, 3), 16) / 255
  const green = Number.parseInt(normalized.slice(3, 5), 16) / 255
  const blue = Number.parseInt(normalized.slice(5, 7), 16) / 255
  const alpha = Number.parseInt(normalized.slice(7, 9), 16) / 255
  const maximum = Math.max(red, green, blue)
  const minimum = Math.min(red, green, blue)
  const delta = maximum - minimum
  let hue = 0

  if (delta > 0) {
    if (maximum === red)
      hue = 60 * (((green - blue) / delta) % 6)
    else if (maximum === green)
      hue = 60 * ((blue - red) / delta + 2)
    else
      hue = 60 * ((red - green) / delta + 4)
  }

  return {
    h: hue < 0 ? hue + 360 : hue,
    s: maximum === 0 ? 0 : (delta / maximum) * 100,
    v: maximum * 100,
    a: alpha,
  }
}

export function hsvaToHex(color: HsvaColor, allowAlpha: boolean) {
  const hue = ((color.h % 360) + 360) % 360
  const saturation = clamp(color.s, 0, 100) / 100
  const brightness = clamp(color.v, 0, 100) / 100
  const chroma = brightness * saturation
  const section = hue / 60
  const intermediate = chroma * (1 - Math.abs((section % 2) - 1))
  const offset = brightness - chroma
  const [red, green, blue] = section < 1
    ? [chroma, intermediate, 0]
    : section < 2
      ? [intermediate, chroma, 0]
      : section < 3
        ? [0, chroma, intermediate]
        : section < 4
          ? [0, intermediate, chroma]
          : section < 5
            ? [intermediate, 0, chroma]
            : [chroma, 0, intermediate]

  const rgb = [red, green, blue]
    .map(channel => Math.round((channel + offset) * 255).toString(16).padStart(2, '0'))
    .join('')
  const alpha = allowAlpha
    ? Math.round(clamp(color.a, 0, 1) * 255).toString(16).padStart(2, '0')
    : ''
  return `#${rgb}${alpha}`.toUpperCase()
}

export function parseRgbaColor(value: string): RgbaColor | null {
  const match = RGBA_COLOR_PATTERN.exec(value.trim())
  if (!match)
    return null

  const red = Number(match[1])
  const green = Number(match[2])
  const blue = Number(match[3])
  const alpha = Number(match[4])
  if ([red, green, blue].some(channel => channel > 255))
    return null

  return { r: red, g: green, b: blue, a: alpha }
}

export function rgbaToHexColor(color: RgbaColor) {
  return `#${[color.r, color.g, color.b, color.a * 255]
    .map(channel => Math.round(channel).toString(16).padStart(2, '0'))
    .join('')}`.toUpperCase()
}

export function formatRgbaColor(value: string) {
  const normalized = normalizePickerHexColor(value, true)
  if (!normalized)
    return null

  const red = Number.parseInt(normalized.slice(1, 3), 16)
  const green = Number.parseInt(normalized.slice(3, 5), 16)
  const blue = Number.parseInt(normalized.slice(5, 7), 16)
  const alpha = Number((Number.parseInt(normalized.slice(7, 9), 16) / 255).toFixed(3))
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value))
}
