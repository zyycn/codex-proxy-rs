<script setup lang="ts">
import type { dashboardSnapshotView } from '../presenter'

import { BaseCard } from '@codex-proxy/ui'
import { usageRecordColumns } from '@/components/usage/shared/columns'
import UsageRecordsTable from '@/components/usage/UsageRecordsTable.vue'

type DashboardSnapshot = ReturnType<typeof dashboardSnapshotView>

defineProps<{
  rows: DashboardSnapshot['usageRecords']
}>()

const dashboardUsageRecordColumns = usageRecordColumns.filter(
  column => column.key !== 'actions' && column.key !== 'clientApiKeyName',
)
</script>

<template>
  <BaseCard
    as="article"
    title="使用记录"
    description="最近 10 条成功请求"
    class="h-117 w-full"
  >
    <template #body>
      <div class="flex h-91 w-full overflow-hidden">
        <UsageRecordsTable
          class="min-w-0 flex-1"
          :columns="dashboardUsageRecordColumns"
          :rows="rows"
          empty-text="暂无成功记录"
        />
      </div>
    </template>
  </BaseCard>
</template>
