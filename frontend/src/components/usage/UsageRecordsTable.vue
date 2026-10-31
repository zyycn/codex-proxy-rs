<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'

import type { UsageListRecord } from '@/api'
import { ZTable } from '@codex-proxy/ui'

import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import UsageBillingCell from '@/components/usage/UsageBillingCell.vue'
import UsageClientIpCell from '@/components/usage/UsageClientIpCell.vue'
import UsageLatencyCell from '@/components/usage/UsageLatencyCell.vue'
import UsageModelCell from '@/components/usage/UsageModelCell.vue'
import UsagePerformanceCell from '@/components/usage/UsagePerformanceCell.vue'
import UsageReasoningEffortCell from '@/components/usage/UsageReasoningEffortCell.vue'
import UsageRouteCell from '@/components/usage/UsageRouteCell.vue'
import UsageTimeCell from '@/components/usage/UsageTimeCell.vue'
import UsageTokenCell from '@/components/usage/UsageTokenCell.vue'
import UsageTransportBadge from '@/components/usage/UsageTransportBadge.vue'
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
        v-if="row.authenticationKind === 'api_key' || row.accountPlanType"
        :authentication-kind="row.authenticationKind ?? undefined"
        :plan-type="row.accountPlanType"
        :plan-type-display="row.accountPlanTypeDisplay || row.accountPlanType || ''"
        size="sm"
      />
      <span v-else class="text-cp-text-quaternary">—</span>
    </template>

    <template #createdAtDisplay="{ row }">
      <UsageTimeCell :value="row.createdAtDisplay" />
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
      <UsageRouteCell :record="row" />
    </template>

    <template #errorMessage="{ row }">
      <div class="min-w-0 py-0.5" :title="row.errorMessage || row.errorCode || undefined">
        <code class="block truncate font-mono text-cp-sm font-bold text-cp-error-text">
          {{ row.errorCode || (row.logicalOutcome === 'incomplete' ? t('usage.error.responseIsNotComplete') : t('usage.error.requestFailed')) }}
        </code>
        <p v-if="row.errorMessage" class="mt-1 mb-0 truncate text-cp-xs leading-[1.45] font-emphasis text-cp-text-secondary">
          {{ row.errorMessage }}
        </p>
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

    <template #performance="{ row }">
      <UsagePerformanceCell :record="row" />
    </template>

    <template v-if="$slots.actions" #actions="scope">
      <slot name="actions" v-bind="scope" />
    </template>
  </ZTable>
</template>
