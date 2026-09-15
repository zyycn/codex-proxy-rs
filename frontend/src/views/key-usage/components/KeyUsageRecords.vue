<script setup lang="ts">
import type { KeyUsageRecord, KeyUsageRecordKind } from '@/api/modules/key-usage'
import type { BaseTablePagination as Pagination } from '@/components/base/BaseTable/pagination'
import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import UsageBillingCell from '@/views/usage/components/UsageBillingCell.vue'
import UsageClientIpCell from '@/views/usage/components/UsageClientIpCell.vue'
import UsageLatencyCell from '@/views/usage/components/UsageLatencyCell.vue'
import UsageTokenCell from '@/views/usage/components/UsageTokenCell.vue'
import UsageTransportBadge from '@/views/usage/components/UsageTransportBadge.vue'
import { keyUsageTime } from '../utils/format'

defineProps<{ rows: KeyUsageRecord[], pagination: Pagination, loading: boolean, error: string, stale: boolean }>()
defineEmits<{ pageChange: [page: number], pageSizeChange: [size: number] }>()
const kind = defineModel<KeyUsageRecordKind>('kind', { required: true })
const columns = computed(() => defineTableColumns<KeyUsageRecord>([
  { key: 'model', label: '模型', kind: 'custom', size: 'xl' },
  { key: 'reasoningEffort', label: '推理强度', kind: 'status', size: 'lg' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'upstreamTransport', label: '上游', kind: 'status', size: 'md' },
  { key: 'clientTransport', label: '接入', kind: 'status', size: 'md' },
  { key: 'tokenDetails', label: 'TOKEN', kind: 'numeric', size: 'xl' },
  { key: 'billing', label: '费用', kind: 'numeric', size: 'lg' },
  { key: 'latency', label: '延迟', kind: 'numeric', size: 'xl' },
  ...(kind.value === 'error' ? [{ key: 'statusCode', label: '状态', kind: 'status' as const }] : []),
  { key: 'createdAt', label: '时间', kind: 'datetime' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl' },
]))
</script>

<template>
  <BaseCard title="请求日志" description="当前请求记录">
    <template #actions>
      <BaseSegmented v-model="kind" label="请求结果" :options="[{ label: '成功请求', value: 'success' }, { label: '错误记录', value: 'error' }]" />
    </template>
    <p v-if="error || stale" role="status" class="mt-0 mb-3 text-cp-sm text-cp-error-text">
      {{ error || '请求日志刷新失败，暂时保留上次结果。' }}
    </p>
    <div class="flex h-120 min-h-0 overflow-hidden">
      <BaseTable class="min-w-0 flex-1" :columns="columns" :rows="rows" :loading="loading" scrollbar-always-visible :empty-text="error ? '请求日志加载失败，请点击顶部刷新重试' : '所选条件下暂无记录'">
        <template #model="{ row }">
          <code class="block max-w-full truncate font-mono text-cp-sm leading-none font-heavy text-cp-text">{{ row.model || '—' }}</code>
        </template>
        <template #reasoningEffort="{ row }">
          <span class="whitespace-nowrap text-cp-sm font-bold text-cp-text">{{ row.reasoningEffort || '—' }}</span>
        </template>
        <template #upstreamTransport="{ row }">
          <UsageTransportBadge :transport="row.upstreamTransport" />
        </template>
        <template #clientTransport="{ row }">
          <UsageTransportBadge :transport="row.clientTransport" />
        </template>
        <template #tokenDetails="{ row }">
          <UsageTokenCell v-if="row.tokenDetails" :record="{ tokenDetails: row.tokenDetails }" />
          <span v-else class="text-cp-text-tertiary">—</span>
        </template>
        <template #billing="{ row }">
          <UsageBillingCell :record="row" />
        </template>
        <template #latency="{ row }">
          <UsageLatencyCell :record="row" />
        </template>
        <template #statusCode="{ row }">
          <span class="font-mono text-cp-sm font-bold text-cp-error-text">{{ row.statusCode ?? '错误' }}</span>
        </template>
        <template #createdAt="{ row }">
          {{ keyUsageTime(row.createdAt) }}
        </template>
        <template #clientIp="{ row }">
          <UsageClientIpCell :record="row" />
        </template>
        <template #userAgent="{ row }">
          <span class="block max-w-full wrap-break-word whitespace-normal font-mono text-cp-sm leading-[1.4] font-emphasis text-cp-text-secondary">{{ row.userAgent || '—' }}</span>
        </template>
      </BaseTable>
    </div>
    <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="$emit('pageChange', $event)" @page-size-change="$emit('pageSizeChange', $event)" />
  </BaseCard>
</template>
