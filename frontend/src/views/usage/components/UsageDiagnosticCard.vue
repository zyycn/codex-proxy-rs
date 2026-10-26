<script setup lang="ts">
import type { TableColumnSize } from '@codex-proxy/ui'

import type { getUsageRecordInsightsDiagnostics } from '@/api'
import { defineTableColumns, ZCard, ZEmpty, ZSegmented, ZTable } from '@codex-proxy/ui'
import { CornerDownRight } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { formatLocalizedCompactNumber as formatCompactNumber, formatDuration, formatPercent, formatUsd } from '@/utils/format'

const props = withDefaults(
  defineProps<{
    diagnostics: Diagnostics
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const { t } = useI18n({ useScope: 'global' })

type Diagnostics = Omit<Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>>, 'analytics'>

const dimension = defineModel('dimension', { type: String, required: true })

const dimensionOptions = computed(() => ([
  {
    label: t('usage.fields.model'),
    value: 'model',
  },
  {
    label: t('accounts.account'),
    value: 'account',
  },
  {
    label: t('usage.diagnostics.key'),
    value: 'apiKey',
  },
  {
    label: t('usage.fields.upstream'),
    value: 'provider',
  },
  {
    label: t('usage.diagnostics.transport'),
    value: 'transport',
  },
  {
    label: t('usage.filters.error'),
    value: 'failureClass',
  },
]))

const resultDimension = computed(() => props.diagnostics.dimension || dimension.value)
const resultDimensionLabel = computed(
  () => dimensionOptions.value.find(option => option.value === resultDimension.value)?.label ?? t('usage.diagnostics.dimensions'),
)

const dimensionNameSizes: Record<string, TableColumnSize> = {
  model: 'xl',
  account: '2xl',
  apiKey: 'md',
  provider: 'lg',
  transport: 'sm',
  failureClass: 'xl',
}

const diagnosticColumns = computed(() => defineTableColumns<DiagnosticDisplayItem>([
  {
    key: 'nameDisplay',
    label: t('usage.diagnostics.dimensions'),
    kind: 'custom',
    size: dimensionNameSizes[resultDimension.value] ?? 'xl',
  },
  ...(resultDimension.value === 'account'
    ? defineTableColumns<DiagnosticDisplayItem>([
        {
          key: 'accountPlanType',
          label: t('usage.fields.subscription'),
          kind: 'status',
          size: 'sm',
          fixedWidth: true,
        },
      ])
    : []),
  {
    key: 'requestCount',
    label: t('usage.diagnostics.request'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'errorCount',
    label: t('common.status.failed'),
    kind: 'numeric',
    size: 'xs',
  },
  {
    key: 'firstTokenP95Ms',
    label: t('usage.diagnostics.performance'),
    kind: 'numeric',
    size: 'lg',
  },
  {
    key: 'estimatedCost',
    label: t('usage.fields.cost'),
    kind: 'numeric',
    size: 'sm',
  },
]))

type DiagnosticDisplayItem = Diagnostics['items'][number] & {
  nameDisplay: ReturnType<typeof diagnosticNameDisplay>
}

const displayItems = computed<DiagnosticDisplayItem[]>(() =>
  props.diagnostics.items.map(item => ({
    ...item,
    nameDisplay: diagnosticNameDisplay(item.name),
  })),
)
const hasData = computed(() => !props.loading && displayItems.value.length > 0)

function diagnosticNameDisplay(name: string) {
  const raw = name.trim() || t('common.values.unknown')
  const full
    = resultDimension.value === 'transport'
      ? ({ websocket: 'WS', http_sse: 'SSE' }[raw] ?? raw)
      : raw
  if (resultDimension.value !== 'model' && resultDimension.value !== 'account') {
    return { primary: full, secondary: '', full }
  }

  const [primary, secondary = ''] = full.split(/\s+(?:→|->)\s+/, 2)
  return {
    primary: primary || full,
    secondary,
    full,
  }
}
</script>

<template>
  <ZCard
    as="article"
    :title="t('usage.diagnostics.title')"
    :description="t('usage.diagnostics.groupBy', { resultDimensionLabel })"
    class="max-h-105 min-w-0 w-full xl:h-full"
  >
    <template #actions>
      <ZSegmented
        v-model="dimension"
        :aria-label="t('usage.diagnostics.diagnosticDimension')"
        :options="dimensionOptions"
        :disabled="loading"
      />
    </template>

    <template #body>
      <ZTable
        v-if="hasData"
        :key="resultDimension"
        class="min-h-0 max-h-80 w-full xl:max-h-none xl:contain-[size]"
        :columns="diagnosticColumns"
        :data="displayItems"
        size="small"
        row-key="key"
        :empty-text="t('usage.diagnostics.noDiagnosticDataYet')"
      >
        <template #header-nameDisplay>
          {{ resultDimensionLabel }}
        </template>

        <template #nameDisplay="{ row }">
          <div class="inline-grid max-w-full min-w-0 gap-1" :title="row.nameDisplay.full">
            <code
              class="block max-w-full truncate font-mono text-cp-sm leading-none font-heavy text-cp-text"
            >
              {{ row.nameDisplay.primary }}
            </code>
            <span
              v-if="row.nameDisplay.secondary"
              class="flex min-w-0 items-center gap-1.25 text-cp-text-secondary"
            >
              <CornerDownRight
                v-if="resultDimension !== 'account'"
                class="size-3.25 shrink-0 text-cp-blue-text"
                stroke-width="2.4"
              />
              <code class="block truncate font-mono text-cp-xs font-bold">
                {{ row.nameDisplay.secondary }}
              </code>
            </span>
          </div>
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

        <template #requestCount="{ row }">
          <span
            class="grid justify-items-end gap-1 font-mono leading-none tabular-nums"
            :title="t('usage.diagnostics.successful', { count: formatCompactNumber(row.successCount) })"
          >
            <strong class="font-bold text-cp-text">
              {{ formatCompactNumber(row.requestCount) }}
            </strong>
            <small class="text-[10px] font-emphasis text-cp-text-quaternary">
              {{ formatPercent(row.requestShare) }}
            </small>
          </span>
        </template>

        <template #errorCount="{ row }">
          <strong
            class="font-mono font-bold tabular-nums"
            :class="row.errorCount > 0 ? 'text-cp-error-text' : 'text-cp-text-quaternary'"
            :aria-label="t('usage.diagnostics.failed', { count: formatCompactNumber(row.errorCount) })"
            :title="t('usage.diagnostics.failureRate', { rate: formatPercent(row.errorRate) })"
          >
            {{ formatCompactNumber(row.errorCount) }}
          </strong>
        </template>

        <template #firstTokenP95Ms="{ row }">
          <div
            class="grid justify-items-end gap-1 font-mono leading-none tabular-nums"
            :title="t('usage.diagnostics.retryRate', { rate: formatPercent(row.retryRate) })"
          >
            <span class="inline-flex items-baseline gap-1.5">
              <small class="text-[10px] font-emphasis text-cp-text-quaternary">TTFT</small>
              <strong class="font-bold text-cp-blue-text">
                {{ formatDuration(row.firstTokenP95Ms) }}
              </strong>
            </span>
            <small class="flex items-center gap-2 text-[10px] font-emphasis text-cp-text-quaternary">
              <span>P95 {{ formatDuration(row.latencyP95Ms) }}</span>
              <span :class="row.retryCount > 0 ? 'text-cp-orange-text' : undefined">
                {{ t('usage.diagnostics.retries', { count: formatCompactNumber(row.retryCount) }) }}
              </span>
            </small>
          </div>
        </template>

        <template #estimatedCost="{ row }">
          <span class="font-mono font-bold tabular-nums text-cp-green-text">
            {{ formatUsd(row.estimatedCost) }}
          </span>
        </template>
      </ZTable>
      <ZEmpty
        v-else
        size="small"
        surface="none"
        :title="loading ? t('usage.diagnostics.loading') : t('usage.diagnostics.empty')"
        class="h-full min-h-40 place-content-center"
      />
    </template>
  </ZCard>
</template>
