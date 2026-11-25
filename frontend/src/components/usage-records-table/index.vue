<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'

import type { UsageListRecord } from '@/api'
import { ZTable } from '@codex-proxy/ui'

import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account-plan-badge/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import {
  usageAccountText,
  usageUserAgent,
} from './presenter'
import UsageBillingCell from './UsageBillingCell.vue'
import UsageClientIpCell from './UsageClientIpCell.vue'
import UsageErrorCell from './UsageErrorCell.vue'
import UsageLatencyCell from './UsageLatencyCell.vue'
import UsageModelCell from './UsageModelCell.vue'
import UsagePerformanceCell from './UsagePerformanceCell.vue'
import UsagePerformanceHeader from './UsagePerformanceHeader.vue'
import UsageReasoningEffortCell from './UsageReasoningEffortCell.vue'
import UsageRouteCell from './UsageRouteCell.vue'
import UsageTimeCell from './UsageTimeCell.vue'
import UsageTokenCell from './UsageTokenCell.vue'
import UsageTransportCell from './UsageTransportCell.vue'

// 使用记录表只负责该领域的单元格呈现；筛选与分页由页面组合。
withDefaults(
  defineProps<{
    columns: TableColumn<UsageListRecord>[]
    rows: UsageListRecord[]
    loading?: boolean
    emptyText?: string
  }>(),
  {
    loading: false,
  },
)

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <ZTable
    :columns="columns"
    :data="rows"
    :loading="loading"
    :empty-text="emptyText ?? t('usage.noUsageRecords')"
  >
    <template #clientApiKeyName="{ displayValue }">
      <span
        class="block max-w-full truncate font-mono text-z-sm leading-none font-bold text-z-text"
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
        class="block max-w-full truncate font-mono text-z-sm leading-none font-bold text-z-text"
        :title="usageAccountText(row)"
      >
        {{ usageAccountText(row) }}
      </span>
      <span
        v-if="row.accountNotes?.trim()"
        class="mt-1 block max-w-full truncate text-z-xs font-emphasis text-z-text-quaternary"
        :title="row.accountNotes"
      >
        {{ row.accountNotes }}
      </span>
    </template>

    <template #accountPlanType="{ row }">
      <div
        v-if="row.accountUpstreamPeer || row.authenticationKind === 'api_key' || row.accountPlanType"
        class="-mx-3 flex min-w-0 justify-center"
      >
        <AccountPlanBadge
          wrap
          :authentication-kind="row.authenticationKind ?? undefined"
          :upstream-peer="row.accountUpstreamPeer"
          :plan-type="row.accountPlanType"
          :plan-type-display="row.accountPlanTypeDisplay || row.accountPlanType || ''"
          size="sm"
        />
      </div>
      <span v-else class="text-z-text-quaternary">—</span>
    </template>

    <template #createdAtDisplay="{ row }">
      <UsageTimeCell :value="row.createdAtDisplay" :datetime="row.createdAt" />
    </template>

    <template #clientIp="{ row }">
      <UsageClientIpCell :record="row" />
    </template>

    <template #userAgent="{ row }">
      <span class="block max-w-full wrap-break-word whitespace-normal font-mono text-z-sm leading-[1.4] font-emphasis text-z-text-secondary">
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
      <UsageRouteCell :record="row" />
    </template>

    <template #errorMessage="{ row }">
      <UsageErrorCell :record="row" />
    </template>

    <template #transport="{ row }">
      <UsageTransportCell :upstream="row.upstreamTransport" :client="row.clientTransport" />
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

    <template #header-performance>
      <UsagePerformanceHeader />
    </template>

    <template #performance="{ row }">
      <UsagePerformanceCell :record="row" />
    </template>

    <template v-if="$slots.actions" #actions="scope">
      <slot name="actions" v-bind="scope" />
    </template>
  </ZTable>
</template>
