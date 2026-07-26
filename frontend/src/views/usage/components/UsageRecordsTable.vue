<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import { Minimize2 } from '@lucide/vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import {
  usageAccountText,
  usageAuthenticationKind,
  usageIsCompact,
  usageRecordType,
  usageRecordTypeClass,
} from '../utils/records'
import UsageBillingCell from './UsageBillingCell.vue'
import UsageClientIpCell from './UsageClientIpCell.vue'
import UsageLatencyCell from './UsageLatencyCell.vue'
import UsageModelCell from './UsageModelCell.vue'
import UsageReasoningEffortCell from './UsageReasoningEffortCell.vue'
import UsageTokenCell from './UsageTokenCell.vue'

// 使用记录表：列单元格渲染的唯一定义，供使用统计页与仪表盘卡片共用。
// 其余 BaseTable 配置（loading/pagination/empty-text/min-width 及分页事件）
// 由 attrs 透传，actions 列仅在调用方提供插槽时渲染。
defineProps<{
  columns: BaseTableColumn<UsageDisplayRecord>[]
  rows: UsageDisplayRecord[]
}>()
</script>

<template>
  <BaseTable :columns="columns" :rows="rows">
    <template #provider="{ row }">
      <ProviderIconGroup
        :provider="String(row.provider || '')"
        :authentication-kind="usageAuthenticationKind(row)"
      />
    </template>

    <template #accountEmail="{ row }">
      <span
        class="block max-w-full truncate font-mono text-[12px] leading-none font-bold text-(--cp-text-primary)"
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
        <code class="font-mono text-[12px] font-emphasis">{{ row.route || '—' }}</code>
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

    <template v-if="$slots.actions" #actions="scope">
      <slot name="actions" v-bind="scope" />
    </template>
  </BaseTable>
</template>
