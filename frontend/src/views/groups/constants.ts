import type { AccountGroup } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { t } from '@/i18n'

export const DEFAULT_ACCOUNT_GROUP_COLOR = '#60A5FA80'

export const ACCOUNT_GROUP_COLOR_PRESETS = [
  '#60A5FA80',
  '#22D3EE80',
  '#2DD4BF80',
  '#4ADE8080',
  '#FBBF2480',
  '#FB718580',
  '#A78BFA80',
  '#94A3B880',
] as const

export const accountGroupColumns = computed(() => (defineTableColumns<AccountGroup>([
  { key: 'selection', kind: 'selection' },
  {
    key: 'identity',
    label: t('groups.group'),
    kind: 'identity',
  },
  {
    key: 'color',
    label: t('common.fields.color'),
    kind: 'status',
    size: 'sm',
  },
  {
    key: 'enabled',
    label: t('common.fields.status'),
    kind: 'status',
  },
  {
    key: 'accountCount',
    label: t('accounts.fields.count'),
    kind: 'custom',
  },
  {
    key: 'capacity',
    label: t('groups.fields.capacity'),
    kind: 'custom',
    size: 'md',
  },
  {
    key: 'usage',
    label: t('usage.usage'),
    kind: 'custom',
  },
  {
    key: 'updatedAtDisplay',
    label: t('common.fields.updatedAt'),
    kind: 'datetime',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'lg',
  },
])))

export const accountGroupStatusOptions = computed(() => ([
  {
    label: t('common.status.all'),
    value: '',
  },
  {
    label: t('common.status.enabled'),
    value: 'enabled',
  },
  {
    label: t('common.status.disabled'),
    value: 'disabled',
  },
]))
