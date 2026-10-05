<script setup lang="ts">
import type { BaseTableColumn } from '@codex-proxy/ui'
import type { UsageListRecord } from '@/api'

import { BaseTable } from '@codex-proxy/ui'
import { Minimize2 } from '@lucide/vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import UsageBillingCell from '@/components/usage/UsageBillingCell.vue'
import UsageClientIpCell from '@/components/usage/UsageClientIpCell.vue'
import UsageLatencyCell from '@/components/usage/UsageLatencyCell.vue'
import UsageModelCell from '@/components/usage/UsageModelCell.vue'
import UsageReasoningEffortCell from '@/components/usage/UsageReasoningEffortCell.vue'
import UsageTokenCell from '@/components/usage/UsageTokenCell.vue'
import UsageTransportBadge from '@/components/usage/UsageTransportBadge.vue'
import {
  usageAccountText,
  usageUserAgent,
} from './shared/presenter'

// 使用记录表只负责该领域的单元格呈现；筛选与分页由页面组合。
withDefaults(
  defineProps<{
    columns: BaseTableColumn<UsageListRecord>[]
    rows: UsageListRecord[]
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
    <template #clientApiKeyName="{ displayValue }">
      <span
        class="block max-w-full truncate font-mono text-cp-sm leading-none font-bold text-cp-text"
        :title="String(displayValue)"
      >
        {{ displayValue }}
      </span>
    </template>

    <template #provider="{ row }">
      <ProviderIconGroup
        :provider="String(row.provider || '')"
        :authentication-kind="row.authenticationKind"
      />
    </template>

    <template #accountEmail="{ row }">
      <span
        class="block max-w-full truncate font-mono text-cp-sm leading-none font-bold text-cp-text"
        :title="usageAccountText(row)"
      >
        {{ usageAccountText(row) }}
      </span>
      <span
        v-if="row.accountNotes?.trim()"
        class="mt-1 block max-w-full truncate text-cp-xs font-emphasis text-cp-text-quaternary"
        :title="row.accountNotes"
      >
        {{ row.accountNotes }}
      </span>
    </template>

    <template #accountPlanType="{ row }">
      <AccountPlanBadge
        v-if="row.accountPlanType"
        :plan-type="row.accountPlanType"
        :plan-type-display="row.accountPlanTypeDisplay || row.accountPlanType"
        size="sm"
      />
      <span v-else class="text-cp-text-quaternary">—</span>
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
          v-if="row.compact"
          class="inline-flex shrink-0 text-cp-orange-text"
          title="压缩请求"
          aria-label="压缩请求"
        >
          <Minimize2 class="size-3.5" stroke-width="2.4" />
        </span>
      </div>
    </template>

    <template #clientTransport="{ row }">
      <UsageTransportBadge :transport="row.clientTransport" />
    </template>

    <template #upstreamTransport="{ row }">
      <UsageTransportBadge :transport="row.upstreamTransport" />
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
