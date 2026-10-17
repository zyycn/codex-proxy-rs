<script setup lang="ts">
import type { AccountConcurrencyEntry } from '@/api'
import { defineTableColumns, ZTable } from '@codex-proxy/ui'
import { computed } from 'vue'
import UsageClientIpCell from '@/components/usage/UsageClientIpCell.vue'
import { formatDuration } from '@/utils/format'

const props = defineProps<{
  entries: AccountConcurrencyEntry[]
  total: number
  pageSize: number
  loading: boolean
  emptyText: string
}>()

const columns = defineTableColumns<AccountConcurrencyEntry>([
  { key: 'key', label: '密钥', kind: 'identity', size: '2xl' },
  { key: 'model', label: '模型', kind: 'mono', size: 'lg' },
  { key: 'clientIp', label: '来源 IP', kind: 'custom', size: '4xl' },
  { key: 'startedAtDisplay', label: '开始时间', kind: 'datetime', size: 'xl' },
  { key: 'occupiedMs', label: '占用时长', kind: 'numeric', size: 'lg', formatter: (_, row) => formatDuration(row.occupiedMs) },
])

// 按分页容量保留高度，切页与末页记录较少时不让弹窗收缩
const tableHeight = computed(() => `min(60dvh, max(16rem, calc(2.5rem + 3px + ${Math.min(props.total, props.pageSize, 10)} * var(--cp-table-row-height))))`)
</script>

<template>
  <ZTable
    class="min-h-0 [--cp-table-row-height:3rem]"
    :style="{ height: tableHeight }"
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
