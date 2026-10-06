export function websocketMaxAgeSeconds(value: number | undefined): string {
  if (value === undefined)
    return ''
  const seconds = Math.floor(value / 1000)
  const milliseconds = value % 1000
  return milliseconds ? `${seconds}.${String(milliseconds).padStart(3, '0').replace(/0+$/, '')}` : String(seconds)
}

export function parseWebsocketMaxAgeSeconds(input: string): { valid: true, value: number | null } | { valid: false, message: string } {
  const text = input.trim()
  if (!text)
    return { valid: true, value: null }
  const match = /^(\d+)(?:\.(\d{1,3}))?$/.exec(text)
  if (match) {
    const value = Number(match[1]) * 1000 + Number((match[2] ?? '').padEnd(3, '0'))
    if (Number.isSafeInteger(value) && value > 0)
      return { valid: true, value }
  }
  return { valid: false, message: 'WS 最大复用时间需为大于 0 且最多三位小数的秒数' }
}
