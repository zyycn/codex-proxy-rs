<script setup lang="ts">
import type { AccountConcurrencyEntry } from '@/api'

import { defineTableColumns, ZTable } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import UsageClientIpCell from '@/components/usage/UsageClientIpCell.vue'
import { formatDuration } from '@/utils/format'

defineProps<{
  entries: AccountConcurrencyEntry[]
  loading: boolean
  emptyText: string
}>()

const { t } = useI18n({ useScope: 'global' })

const columns = computed(() => (defineTableColumns<AccountConcurrencyEntry>([
  {
    key: 'key',
    label: t('accounts.concurrency.key'),
    kind: 'identity',
    size: '2xl',
  },
  {
    key: 'model',
    label: t('accounts.concurrency.model'),
    kind: 'mono',
    size: 'lg',
  },
  {
    key: 'clientIp',
    label: t('accounts.concurrency.sourceIp'),
    kind: 'custom',
    size: '4xl',
  },
  {
    key: 'startedAtDisplay',
    label: t('accounts.concurrency.startTime'),
    kind: 'datetime',
    size: 'xl',
  },
  {
    key: 'occupiedMs',
    label: t('accounts.concurrency.elapsed'),
    kind: 'numeric',
    size: 'lg',
    formatter: (_, row) => formatDuration(row.occupiedMs),
  },
])))
</script>

<template>
  <ZTable
    class="h-auto! min-h-[min(60dvh,20rem)] [--z-table-row-height:3rem]"
    :columns="columns" :data="entries" row-key="leaseId"
    :loading="loading" :empty-text="emptyText"
  >
    <template #key="{ row }">
      <span
        class="block truncate font-emphasis text-z-text"
        :title="[row.clientKeyName, row.clientKeyId].filter(Boolean).join('\n') || undefined"
      >
        {{ row.clientKeyName ?? row.clientKeyId ?? '—' }}
      </span>
    </template>
    <template #model="{ displayValue }">
      <span class="text-z font-normal text-z-text-secondary" :title="String(displayValue)">{{ displayValue }}</span>
    </template>
    <template #startedAtDisplay="{ displayValue }">
      <span class="text-z">{{ displayValue }}</span>
    </template>
    <template #clientIp="{ row }">
      <UsageClientIpCell v-if="row.clientIp" :record="row" class="text-z!" />
      <span v-else class="text-z-text-quaternary">—</span>
    </template>
  </ZTable>
</template>
