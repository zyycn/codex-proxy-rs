import type { OutboundProxyRecord } from '@/api'

export function effectiveProxyLocation(proxy: OutboundProxyRecord) {
  return proxy.autoLocation ? proxy.detectedLocation?.location : proxy.location
}
