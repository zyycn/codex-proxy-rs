<script setup lang="ts">
import type { PaginationState as Pagination } from '@codex-proxy/ui'

import type { KeyUsageRecord, KeyUsageRecordKind } from '@/api'
import { defineTableColumns, useTableColumns, ZCard, ZPagination, ZTable, ZTableColumnSettings } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageDataColumns } from '@/components/usage-records-table/columns'
import UsageBillingCell from '@/components/usage-records-table/UsageBillingCell.vue'
import UsageClientIpCell from '@/components/usage-records-table/UsageClientIpCell.vue'
import UsageLatencyCell from '@/components/usage-records-table/UsageLatencyCell.vue'
import UsageModelCell from '@/components/usage-records-table/UsageModelCell.vue'
import UsagePerformanceCell from '@/components/usage-records-table/UsagePerformanceCell.vue'
import UsagePerformanceHeader from '@/components/usage-records-table/UsagePerformanceHeader.vue'
import UsageReasoningEffortCell from '@/components/usage-records-table/UsageReasoningEffortCell.vue'
import UsageRouteCell from '@/components/usage-records-table/UsageRouteCell.vue'
import UsageTimeCell from '@/components/usage-records-table/UsageTimeCell.vue'
import UsageTokenCell from '@/components/usage-records-table/UsageTokenCell.vue'
import UsageTransportCell from '@/components/usage-records-table/UsageTransportCell.vue'
import UsageRecordStatusFilter from '@/components/UsageRecordStatusFilter.vue'

defineProps<{ rows: KeyUsageRecord[], pagination: Pagination, loading: boolean }>()

defineEmits<{ pageChange: [page: number], pageSizeChange: [size: number] }>()

const { t } = useI18n({ useScope: 'global' })

const kind = defineModel<KeyUsageRecordKind>('kind', { required: true })
const columns = computed(() => defineTableColumns<KeyUsageRecord>(usageDataColumns.value))
const { visibleColumns, columnOptions, setColumnVisible, setColumnOrder, resetColumns } = useTableColumns(columns, 'key-usage-records')
</script>

<template>
  <ZCard :title="t('usage.records.requestLog')">
    <template #actions>
      <div class="flex items-center gap-2">
        <UsageRecordStatusFilter v-model="kind" />
        <ZTableColumnSettings :options="columnOptions" @change="setColumnVisible" @reorder="setColumnOrder" @reset="resetColumns" />
      </div>
    </template>
    <div class="flex h-120 min-h-0 overflow-hidden">
      <ZTable class="min-w-0 flex-1" :columns="visibleColumns" :data="rows" :loading="loading" :empty-text="t('usage.records.noRecordsMatchTheseFilters')">
        <template #model="{ row }">
          <UsageModelCell :record="row" />
        </template>
        <template #reasoningEffort="{ row }">
          <UsageReasoningEffortCell :record="row" />
        </template>
        <template #route="{ row }">
          <UsageRouteCell :record="row" />
        </template>
        <template #transport="{ row }">
          <UsageTransportCell :upstream="row.upstreamTransport" :client="row.clientTransport" />
        </template>
        <template #tokenDetails="{ row }">
          <UsageTokenCell v-if="row.tokenDetails" :record="{ tokenDetails: row.tokenDetails }" />
          <span v-else class="text-z-text-tertiary">—</span>
        </template>
        <template #billing="{ row }">
          <UsageBillingCell :record="row" />
        </template>
        <template #latency="{ row }">
          <UsageLatencyCell :record="row" />
        </template>
        <template #header-performance>
          <UsagePerformanceHeader />
        </template>
        <template #performance="{ row }">
          <UsagePerformanceCell :record="row" />
        </template>
        <template #createdAtDisplay="{ row }">
          <UsageTimeCell :value="row.createdAtDisplay" :datetime="row.createdAt" />
        </template>
        <template #clientIp="{ row }">
          <UsageClientIpCell :record="row" />
        </template>
        <template #userAgent="{ row }">
          <span class="block max-w-full wrap-break-word whitespace-normal font-mono text-z-sm leading-[1.4] font-emphasis text-z-text-secondary">{{ row.userAgent || '—' }}</span>
        </template>
      </ZTable>
    </div>
    <ZPagination v-bind="pagination" :disabled="loading" @current-change="$emit('pageChange', $event)" @size-change="$emit('pageSizeChange', $event)" />
  </ZCard>
</template>
