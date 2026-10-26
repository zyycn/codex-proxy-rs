import type { TableColumn } from '@codex-proxy/ui'
import type { UsageListRecord } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { t } from '@/i18n'
import { formatProviderLabel } from '@/utils/providers'

export const usageDataColumns = computed(() => ([
  {
    key: 'model',
    label: t('usage.fields.model'),
    kind: 'custom',
    size: 'lg',
  },
  {
    key: 'reasoningEffort',
    label: t('usage.fields.reasoningEffort'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'route',
    label: t('usage.fields.endpoint'),
    kind: 'mono',
  },
  {
    key: 'upstreamTransport',
    label: t('usage.fields.upstream'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'clientTransport',
    label: t('usage.fields.access'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'tokenDetails',
    label: 'TOKEN',
    kind: 'numeric',
    size: 'xl',
  },
  {
    key: 'billing',
    label: t('usage.fields.cost'),
    kind: 'numeric',
    size: 'xl',
  },
  {
    key: 'latency',
    label: t('usage.fields.latency'),
    kind: 'numeric',
    size: 'xl',
  },
  {
    key: 'performance',
    label: t('usage.outputThroughput'),
    kind: 'numeric',
    size: 'lg',
  },
  {
    key: 'createdAtDisplay',
    label: t('usage.fields.time'),
    kind: 'datetime',
  },
  {
    key: 'clientIp',
    label: 'IP',
    kind: 'custom',
    size: '3xl',
  },
  {
    key: 'userAgent',
    label: 'User-Agent',
    kind: 'custom',
    size: '4xl',
  },
] satisfies TableColumn[]))

export const usageRecordColumns = computed(() => (defineTableColumns<UsageListRecord>([
  {
    key: 'clientApiKeyName',
    label: t('usage.fields.keyName'),
    kind: 'identity',
    size: 'xl',
  },
  {
    key: 'accountEmail',
    hideable: false,
    label: t('accounts.account'),
    kind: 'identity',
    size: '3xl',
  },
  {
    key: 'accountPlanType',
    label: t('usage.fields.subscription'),
    kind: 'status',
    size: 'sm',
    fixedWidth: true,
  },
  {
    key: 'provider',
    label: t('usage.fields.platformType'),
    kind: 'status',
    size: 'md',
    fixedWidth: true,
    formatter: (value: unknown) => formatProviderLabel(typeof value === 'string'
      ? value
      : null),
  },
  ...usageDataColumns.value,
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'sm',
    hideable: false,
  },
])))
