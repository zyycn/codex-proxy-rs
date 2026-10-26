import type { OutboundProxyRecord } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { t } from '@/i18n'

export const proxyColumns = computed(() => (defineTableColumns<OutboundProxyRecord>([
  { key: 'selection', kind: 'selection' },
  {
    key: 'name',
    label: t('proxies.editor.nameLabel'),
    kind: 'identity',
    size: 'lg',
  },
  {
    key: 'address',
    label: t('proxies.editor.proxyAddress'),
    kind: 'identity',
    size: 'xl',
  },
  {
    key: 'status',
    label: t('common.fields.status'),
    kind: 'status',
  },
  {
    key: 'exitIp',
    label: t('proxies.fields.exitIp'),
    kind: 'custom',
    size: 'xl',
  },
  {
    key: 'region',
    label: t('common.location.region'),
    kind: 'custom',
    size: 'lg',
  },
  {
    key: 'latency',
    label: t('proxies.fields.latency'),
    kind: 'custom',
    size: 'sm',
  },
  {
    key: 'accounts',
    label: t('accounts.fields.count'),
    kind: 'custom',
    size: 'sm',
  },
  {
    key: 'testedAt',
    label: t('proxies.fields.testTime'),
    kind: 'datetime',
    fixedWidth: true,
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'lg',
    fixedWidth: true,
  },
])))
