<script setup lang="ts">
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import BaseCard from '@/components/base/BaseCard.vue'
import UsageRecordsTable from '@/components/business/UsageRecordsTable.vue'

defineProps<{
  columns: BaseTableColumn<UsageDisplayRecord>[]
  rows: UsageDisplayRecord[]
}>()
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
          :columns="columns"
          :rows="rows"
          empty-text="暂无成功记录"
        />
      </div>
    </template>
  </BaseCard>
</template>
