<script setup lang="ts">
import type { dashboardSnapshotView } from '../composables/presenter'

import { Minimize2 } from '@lucide/vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderBadge from '@/components/ProviderBadge.vue'
import UsageBillingCell from '@/views/usage/components/UsageBillingCell.vue'
import UsageClientIpCell from '@/views/usage/components/UsageClientIpCell.vue'
import UsageLatencyCell from '@/views/usage/components/UsageLatencyCell.vue'
import UsageModelCell from '@/views/usage/components/UsageModelCell.vue'
import UsageReasoningEffortCell from '@/views/usage/components/UsageReasoningEffortCell.vue'
import UsageTokenCell from '@/views/usage/components/UsageTokenCell.vue'
import {
  usageAccountText,
  usageIsCompact,
  usageRecordColumns,
  usageRecordType,
  usageRecordTypeClass,
} from '@/views/usage/constants'

type DashboardSnapshot = ReturnType<typeof dashboardSnapshotView>

defineProps<{
  rows: DashboardSnapshot['usageRecords']
}>()

const dashboardUsageRecordColumns = usageRecordColumns.filter(column => column.key !== 'actions')
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    title="使用记录"
    description="最近 10 条成功请求"
    class="h-117 w-full"
  >
    <template #body>
      <div class="mt-4.25 flex h-91 w-full overflow-hidden">
        <BaseTable
          class="min-w-0 flex-1"
          :columns="dashboardUsageRecordColumns"
          :rows="rows"
          empty-text="暂无成功记录"
          min-width="1824px"
        >
          <template #provider="{ row }">
            <ProviderBadge :provider="String(row.provider || '')" />
          </template>

          <template #accountEmail="{ row }">
            <span
              class="block max-w-full truncate font-mono text-[12px] leading-none font-[720] text-(--cp-text-primary)"
              :title="usageAccountText(row)"
            >
              {{ usageAccountText(row) }}
            </span>
          </template>

          <template #clientIp="{ row }">
            <UsageClientIpCell :record="row" />
          </template>

          <template #model="{ row }">
            <UsageModelCell :record="row" />
          </template>

          <template #reasoningEffort="{ row }">
            <UsageReasoningEffortCell :record="row" />
          </template>

          <template #route="{ row }">
            <div class="inline-flex max-w-full items-center gap-1.5 whitespace-nowrap">
              <code class="font-mono text-[12px] font-[650]">{{ row.route || '—' }}</code>
              <span
                v-if="usageIsCompact(row)"
                class="inline-flex shrink-0 text-(--cp-warning-text)"
                title="压缩请求"
                aria-label="压缩请求"
              >
                <Minimize2 class="size-3.5" stroke-width="2.4" />
              </span>
            </div>
          </template>

          <template #recordType="{ row }">
            <span
              class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-[12px] leading-none font-bold"
              :class="usageRecordTypeClass(row)"
            >
              {{ usageRecordType(row) }}
            </span>
          </template>

          <template #tokenDetails="{ row }">
            <UsageTokenCell :record="row" />
          </template>

          <template #billing="{ row }">
            <UsageBillingCell :record="row" />
          </template>

          <template #latency="{ row }">
            <UsageLatencyCell :record="row" />
          </template>
        </BaseTable>
      </div>
    </template>
  </BaseCard>
</template>
