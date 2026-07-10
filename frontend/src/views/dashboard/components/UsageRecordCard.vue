<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
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

defineProps<{
  rows: any[]
}>()

const dashboardUsageRecordColumns = usageRecordColumns.filter((column) => column.key !== 'actions')
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
