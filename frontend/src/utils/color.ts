const RGBA_HEX_COLOR_PATTERN = /^#[0-9A-F]{8}$/i

export function normalizeRgbaHexColor(value: string) {
  const normalized = value.trim().toUpperCase()
  return RGBA_HEX_COLOR_PATTERN.test(normalized) ? normalized : null
}
