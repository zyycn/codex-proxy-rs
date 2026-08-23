const HEX_COLOR_PATTERN = /^#[0-9A-F]{6}$/i
const RGBA_HEX_COLOR_PATTERN = /^#[0-9A-F]{8}$/i

export function normalizeHexColor(value: unknown) {
  if (typeof value !== 'string')
    return null
  const normalized = value.trim().toUpperCase()
  return HEX_COLOR_PATTERN.test(normalized) ? normalized : null
}

export function normalizeRgbaHexColor(value: unknown) {
  if (typeof value !== 'string')
    return null
  const normalized = value.trim().toUpperCase()
  return RGBA_HEX_COLOR_PATTERN.test(normalized) ? normalized : null
}
