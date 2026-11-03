<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'

import type { UsageListRecord } from '@/api'
import { ZTable } from '@codex-proxy/ui'

import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import UsageBillingCell from '@/components/usage/UsageBillingCell.vue'
import UsageClientIpCell from '@/components/usage/UsageClientIpCell.vue'
import UsageErrorCell from '@/components/usage/UsageErrorCell.vue'
import UsageLatencyCell from '@/components/usage/UsageLatencyCell.vue'
import UsageModelCell from '@/components/usage/UsageModelCell.vue'
import UsagePerformanceCell from '@/components/usage/UsagePerformanceCell.vue'
import UsageReasoningEffortCell from '@/components/usage/UsageReasoningEffortCell.vue'
import UsageRouteCell from '@/components/usage/UsageRouteCell.vue'
import UsageTimeCell from '@/components/usage/UsageTimeCell.vue'
import UsageTokenCell from '@/components/usage/UsageTokenCell.vue'
import UsageTransportCell from '@/components/usage/UsageTransportCell.vue'
import {
  usageAccountText,
  usageUserAgent,
} from './shared/presenter'

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
      <AccountPlanBadge
        v-if="row.accountUpstreamPeer || row.authenticationKind === 'api_key' || row.accountPlanType"
        :authentication-kind="row.authenticationKind ?? undefined"
        :upstream-peer="row.accountUpstreamPeer"
        :plan-type="row.accountPlanType"
        :plan-type-display="row.accountPlanTypeDisplay || row.accountPlanType || ''"
        size="sm"
      />
      <span v-else class="text-z-text-quaternary">—</span>
    </template>

    <template #createdAtDisplay="{ row }">
      <UsageTimeCell :value="row.createdAtDisplay" />
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

    <template #performance="{ row }">
      <UsagePerformanceCell :record="row" />
    </template>

    <template v-if="$slots.actions" #actions="scope">
      <slot name="actions" v-bind="scope" />
    </template>
  </ZTable>
</template>
