import type { OutboundProxyRecord } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'

export const proxyColumns = defineTableColumns<OutboundProxyRecord>([
  { key: 'name', label: '代理名称', kind: 'identity', size: 'lg' },
  { key: 'address', label: '代理地址', kind: 'identity', size: 'xl' },
  { key: 'exitIp', label: '出口 IP', kind: 'custom', size: 'xl' },
  { key: 'latency', label: '耗时', kind: 'custom', size: 'sm' },
  { key: 'accounts', label: '关联账号', kind: 'custom', size: 'sm' },
  { key: 'testedAt', label: '测试时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg', fixedWidth: true },
])
