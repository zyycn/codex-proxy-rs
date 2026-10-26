import type { OutboundProxyRecord } from '@/api'
import { numberLocale } from '@/utils/format'

const flags = import.meta.glob<string>('/node_modules/circle-flags/flags/??.svg', { eager: true, import: 'default', query: '?url&no-inline' })

export function proxyExitLocation(proxy: OutboundProxyRecord) {
  const detected = proxy.detectedLocation
  const test = proxy.lastTest
  if (!detected || !test?.success || test.location.status === 'failed' || test.location.status === 'conflict')
    return null
  // 未请求解析的测试也可能换出口，旧地区不能套到新的 IP 上
  if (test.exitIpv4 !== detected.exitIpv4 || test.exitIpv6 !== detected.exitIpv6)
    return null
  return detected.location
}

export function countryName(code: string): string {
  return /^[A-Z]{2}$/u.test(code)
    ? new Intl.DisplayNames([
      numberLocale(),
    ], { type: 'region', fallback: 'code' }).of(code) ?? code
    : code
}

export function countryFlagUrl(code: string): string | undefined {
  return /^[A-Z]{2}$/u.test(code)
    ? flags[`/node_modules/circle-flags/flags/${code.toLowerCase()}.svg`]
    : undefined
}
