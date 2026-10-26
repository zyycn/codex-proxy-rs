import type { ApiKey } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'

import { t } from '@/i18n'

export const apiKeyColumns = computed(() => (defineTableColumns<ApiKey>([
  { key: 'selection', kind: 'selection' },
  {
    key: 'identity',
    label: t('common.fields.name'),
    kind: 'identity',
    size: 'xl',
    sortable: 'name',
  },
  {
    key: 'prefix',
    label: t('keys.fields.keyPrefix'),
    kind: 'mono',
    size: 'xl',
  },
  {
    key: 'enabled',
    label: t('common.fields.status'),
    kind: 'status',
    sortable: true,
  },
  {
    key: 'scope',
    label: t('groups.group'),
    kind: 'status',
    size: 'lg',
  },
  {
    key: 'budget',
    label: t('keys.fields.quotaUsage'),
    kind: 'text',
    size: 'xl',
  },
  {
    key: 'limits',
    label: t('keys.fields.requestLimits'),
    kind: 'text',
    size: 'xl',
  },
  {
    key: 'createdAtDisplay',
    label: t('common.fields.createdAt'),
    kind: 'datetime',
    sortable: 'createdAt',
  },
  {
    key: 'lastUsedAt',
    label: t('keys.fields.lastUsed'),
    kind: 'datetime',
    size: 'md',
    sortable: true,
    emptyText: '',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'lg',
  },
])))
