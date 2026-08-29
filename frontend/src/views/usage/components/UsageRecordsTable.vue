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
  usageUserAgent,
} from '../utils/records'
import UsageBillingCell from './UsageBillingCell.vue'
import UsageClientIpCell from './UsageClientIpCell.vue'
import UsageLatencyCell from './UsageLatencyCell.vue'
import UsageModelCell from './UsageModelCell.vue'
import UsageReasoningEffortCell from './UsageReasoningEffortCell.vue'
import UsageTokenCell from './UsageTokenCell.vue'

// 使用记录表只负责该领域的单元格呈现；筛选与分页由页面组合。
withDefaults(
  defineProps<{
    columns: BaseTableColumn<UsageDisplayRecord>[]
    rows: UsageDisplayRecord[]
    loading?: boolean
    emptyText?: string
  }>(),
  {
    loading: false,
    emptyText: '暂无使用记录',
  },
)
</script>

<template>
  <BaseTable
    :columns="columns"
    :rows="rows"
    :loading="loading"
    :empty-text="emptyText"
  >
    <template #provider="{ row }">
      <ProviderIconGroup
        :provider="String(row.provider || '')"
        :authentication-kind="usageAuthenticationKind(row)"
      />
    </template>

    <template #accountEmail="{ row }">
      <span
        class="block max-w-full truncate font-mono text-cp-sm leading-none font-bold text-cp-text"
        :title="usageAccountText(row)"
      >
        {{ usageAccountText(row) }}
      </span>
    </template>

    <template #clientIp="{ row }">
      <UsageClientIpCell :record="row" />
    </template>

    <template #userAgent="{ row }">
      <span class="block max-w-full wrap-break-word whitespace-normal font-mono text-cp-sm leading-[1.4] font-emphasis text-cp-text-secondary">
        {{ usageUserAgent(row) }}
      </span>
    </template>

    <template #model="{ row }">
      <UsageModelCell :record="row" />
    </template>

    <template #reasoningEffort="{ row }">
      <UsageReasoningEffortCell :record="row" />
    </template>

    <template #route="{ row }">
      <div class="inline-flex max-w-full items-center gap-1.5 whitespace-nowrap">
        <code class="font-mono text-cp-sm font-emphasis">{{ row.route || '—' }}</code>
        <span
          v-if="usageIsCompact(row)"
          class="inline-flex shrink-0 text-cp-orange-text"
          title="压缩请求"
          aria-label="压缩请求"
        >
          <Minimize2 class="size-3.5" stroke-width="2.4" />
        </span>
      </div>
    </template>

    <template #recordType="{ row }">
      <span
        class="inline-flex h-6 min-w-12 items-center justify-center rounded-full px-2 text-cp leading-none font-bold"
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
