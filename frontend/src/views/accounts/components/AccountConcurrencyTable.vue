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
    class="h-[min(60dvh,20rem)]! min-h-0 [--cp-table-row-height:3rem]"
    :columns="columns" :data="entries" row-key="leaseId"
    :loading="loading" :empty-text="emptyText"
  >
    <template #key="{ row }">
      <span
        class="block truncate font-emphasis text-cp-text"
        :title="[row.clientKeyName, row.clientKeyId].filter(Boolean).join('\n') || undefined"
      >
        {{ row.clientKeyName ?? row.clientKeyId ?? '—' }}
      </span>
    </template>
    <template #model="{ displayValue }">
      <span class="text-cp font-normal text-cp-text-secondary" :title="String(displayValue)">{{ displayValue }}</span>
    </template>
    <template #startedAtDisplay="{ displayValue }">
      <span class="text-cp">{{ displayValue }}</span>
    </template>
    <template #clientIp="{ row }">
      <UsageClientIpCell v-if="row.clientIp" :record="row" class="text-cp!" />
      <span v-else class="text-cp-text-quaternary">—</span>
    </template>
  </ZTable>
</template>
