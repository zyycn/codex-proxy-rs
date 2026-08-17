<script setup lang="ts">
import type { dashboardSnapshotView } from '../composables/useDashboard'

import BaseCard from '@/components/base/BaseCard.vue'
import UsageRecordsTable from '@/views/usage/components/UsageRecordsTable.vue'
import { usageRecordColumns } from '@/views/usage/constants'

type DashboardSnapshot = ReturnType<typeof dashboardSnapshotView>

defineProps<{
  rows: DashboardSnapshot['usageRecords']
}>()

const dashboardUsageRecordColumns = usageRecordColumns.filter(column => column.key !== 'actions')
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
