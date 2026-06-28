<script setup lang="ts">
import { computed, shallowRef } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import {
  usageAccountText,
  usageReasoningEffort,
  usageRecordColumns,
  usageRecordType,
  usageRecordTypeClass,
} from '@/views/usage/constants'
import UsageClientIpCell from '@/views/usage/components/UsageClientIpCell.vue'
import UsageCostCell from '@/views/usage/components/UsageCostCell.vue'
import UsageModelCell from '@/views/usage/components/UsageModelCell.vue'
import UsageTokenCell from '@/views/usage/components/UsageTokenCell.vue'

const props = defineProps<{
  rows: any[]
}>()

const activeFilter = shallowRef('all')
const filters = [
  { label: '全部', value: 'all' },
  { label: '错误', value: 'error' },
]
const dashboardUsageRecordColumns = usageRecordColumns.filter((column) => column.key !== 'actions')

const filteredRows = computed(() => {
  if (activeFilter.value === 'error') {
    return props.rows.filter((row) => row.level === 'error' || Number(row.statusCode || 0) >= 400)
  }

  return props.rows
})
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    title="使用记录"
    description="最近 10 条网关请求"
    class="h-87.5 w-full"
  >
    <template #actions>
      <BaseSegmented v-model="activeFilter" :options="filters" class="w-38" />
    </template>

    <template #body>
      <div class="mt-4.25 flex h-60 w-full overflow-hidden">
        <BaseTable
          class="min-w-0 flex-1"
          :columns="dashboardUsageRecordColumns"
          :rows="filteredRows"
          :empty-text="rows.length === 0 ? '暂无使用记录' : '没有匹配记录'"
          min-width="1824px"
        >
          <template #accountEmail="{ row }">
            <span
              class="whitespace-nowrap font-mono text-[12px] leading-none font-[720] text-(--cp-text-primary)"
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
            <span class="whitespace-nowrap text-[12px] font-[720] text-(--cp-text-primary)">
              {{ usageReasoningEffort(row) }}
            </span>
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

          <template #costDetails="{ row }">
            <UsageCostCell :record="row" />
          </template>
        </BaseTable>
      </div>
    </template>
  </BaseCard>
</template>
