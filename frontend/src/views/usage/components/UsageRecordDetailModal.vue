<script setup lang="ts">
import type { EChartsOption } from 'echarts'

import type { UsageRecordDetail } from '@/api'
import { defineTableColumns, ZButton, ZDialog, ZTable } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import {
  usageAccountText,
  usageBillingText,
  usageClientIp,
  usageLatencyDetails,
  usageModelDisplay,
  usageReasoningEffort,
  usageTransportType,
  usageUserAgent,
} from '@/components/usage/shared/presenter'
import { useChartPalette } from '@/composables/useChartPalette'

import { formatDuration, formatInteger } from '@/utils/format'
import { attemptOutcomeClass, attemptOutcomeText, displayValue, fieldLabelClass, fieldValueBaseClass, visibleRequestText, visibleResponseText } from '../utils/detail'
import DiagnosticsDownloadButton from './DiagnosticsDownloadButton.vue'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  record: UsageRecordDetail | null
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })

const { palette } = useChartPalette()

const requestText = computed(() => props.record
  ? visibleRequestText(props.record)
  : '')
const responseText = computed(() => props.record
  ? visibleResponseText(props.record)
  : '')
const modelDisplay = computed(() => props.record
  ? usageModelDisplay(props.record)
  : { primary: '—', secondary: '' })
const tokenDetails = computed(() => props.record?.tokenDetails ?? null)
const billing = computed(() => props.record ? props.record.billing : null)
const latencyDetails = computed(() => props.record
  ? usageLatencyDetails(props.record)
  : null)

const panelClass = 'min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'

const accountDisplay = computed(() => props.record
  ? usageAccountText(props.record)
  : '—')
const finalAttemptIndex = computed(() => {
  const attempts = props.record?.attempts ?? []
  const last = attempts[attempts.length - 1]
  return last?.attemptIndex ?? props.record?.attemptCount
})
const requestItems = computed(() => [
  {
    label: t('accounts.account'),
    value: accountDisplay.value,
    mono: true,
    fullWidth: true,
  },
  {
    label: t('usage.request.statusCode'),
    value: props.record?.statusCode,
    mono: true,
  },
  {
    label: t('usage.request.message'),
    value: props.record?.message,
  },
  {
    label: t('usage.fields.time'),
    value: props.record?.createdAtDisplay,
    mono: true,
  },
  {
    label: t('usage.fields.endpoint'),
    value: props.record?.route,
    mono: true,
  },
])
const overviewItems = computed(() => [
  {
    label: t('usage.error.clientTransfer'),
    value: usageTransportType(props.record?.clientTransport),
    mono: true,
  },
  {
    label: t('usage.error.upstreamTransport'),
    value: usageTransportType(props.record?.upstreamTransport),
    mono: true,
  },
  {
    label: t('usage.latency.totalLatency'),
    value: props.record?.latencyMsDisplay,
    mono: true,
  },
  {
    label: t('usage.request.upstreamTimeConsumption'),
    value: latencyDetails.value?.upstreamDisplay ?? '—',
    mono: true,
  },
  {
    label: t('usage.status.firstOutputWaiting'),
    value: latencyDetails.value?.firstOutputDisplay ?? '—',
    mono: true,
  },
  {
    label: t('usage.tokens.totalTokens'),
    value: tokenDetails.value?.totalTokensDisplay,
    mono: true,
  },
])

const modelRouteItems = computed(() => [
  {
    label: t('usage.fields.endpoint'),
    value: props.record?.route,
    mono: true,
  },
  {
    label: t('usage.fields.reasoningEffort'),
    value: props.record
      ? usageReasoningEffort(props.record)
      : '—',
  },
  {
    label: t('usage.error.requestModel'),
    value: modelDisplay.value.primary,
    mono: true,
  },
  {
    label: t('usage.error.upstreamModel'),
    value: modelDisplay.value.secondary || props.record?.upstreamModel,
    mono: true,
  },
  {
    label: t('usage.request.storageModel'),
    value: props.record?.model,
    mono: true,
  },
])

const clientUpstreamItems = computed(() => [
  {
    label: t('usage.request.clientIp'),
    value: props.record
      ? usageClientIp(props.record)
      : '—',
    mono: true,
  },
  {
    label: t('usage.billing.serviceLevel'),
    value: props.record?.serviceTier,
    mono: true,
  },
  {
    label: t('usage.request.eventType'),
    value: props.record?.kind,
    mono: true,
  },
  {
    label: t('usage.request.trySerialNumber'),
    value: finalAttemptIndex.value,
  },
  {
    label: 'User-Agent',
    value: props.record
      ? usageUserAgent(props.record)
      : '',
    mono: true,
    fullWidth: true,
  },
])

const identifierItems = computed(() => [
  {
    label: t('usage.error.requestId'),
    value: props.record?.requestId,
    mono: true,
    wrap: true,
    fullWidth: true,
  },
  {
    label: t('usage.error.responseId'),
    value: props.record?.responseId,
    mono: true,
    wrap: true,
  },
  {
    label: t('usage.error.upstreamRequestId'),
    value: props.record?.upstreamRequestId,
    mono: true,
    wrap: true,
  },
  {
    label: t('usage.request.accountId'),
    value: props.record?.accountId,
    mono: true,
    wrap: true,
  },
  {
    label: t('usage.error.clientKeyId'),
    value: props.record?.clientApiKeyId,
    mono: true,
    wrap: true,
  },
])

interface AttemptRow {
  id: string
  attemptIndex: number
  outcome: string
  provider: string
  model: string | null
  transport: string
  statusCode: number | null
  accountLabel: string
  accountId: string | null
  latencyMs: number | null
}

const attemptColumns = computed(() => (defineTableColumns<AttemptRow>([
  {
    key: 'attemptIndex',
    label: t('usage.request.serialNumber'),
    kind: 'index',
  },
  {
    key: 'outcome',
    label: t('usage.request.result'),
    kind: 'status',
    size: 'sm',
  },
  {
    key: 'provider',
    label: t('usage.error.platform'),
    kind: 'meta',
    size: 'sm',
  },
  {
    key: 'model',
    label: t('usage.fields.model'),
    kind: 'mono',
    size: 'lg',
  },
  {
    key: 'transport',
    label: t('usage.diagnostics.transport'),
    kind: 'status',
    size: 'md',
  },
  {
    key: 'statusCode',
    label: t('common.fields.status'),
    kind: 'status',
    size: 'sm',
  },
  {
    key: 'accountLabel',
    label: t('accounts.account'),
    kind: 'mono',
    size: '2xl',
  },
  {
    key: 'latencyMs',
    label: t('usage.error.duration'),
    kind: 'numeric',
    size: 'sm',
  },
])))

const attemptRows = computed<AttemptRow[]>(() =>
  (props.record?.attempts ?? []).map(attempt => ({
    id: attempt.id,
    attemptIndex: attempt.attemptIndex,
    outcome: attempt.outcome,
    provider: attempt.provider,
    model: attempt.model,
    transport: attempt.transport,
    statusCode: attempt.statusCode,
    accountLabel: attempt.accountEmail || attempt.accountName || attempt.accountId || '—',
    accountId: attempt.accountId,
    latencyMs: attempt.latencyMs,
  })),
)

const billingItems = computed(() => {
  const value = billing.value
  if (!value) {
    return [
      {
        label: t('usage.billing.totalCost'),
        value: props.record
          ? usageBillingText(props.record)
          : '—',
        mono: true,
      },
    ]
  }

  return [
    {
      label: t('usage.billing.totalCost'),
      value: value.totalAmountDisplay,
      mono: true,
    },
    ...(value.image
      ? [
          {
            label: t('usage.billing.imageInputFee'),
            value: value.image.inputAmountDisplay,
            mono: true,
          },
          {
            label: t('usage.billing.imageCachingCharges'),
            value: value.image.cacheReadAmountDisplay,
            mono: true,
          },
          {
            label: t('usage.billing.imageInputUnitPrice'),
            value: value.image.inputPriceDisplay,
            mono: true,
          },
          {
            label: t('usage.billing.imageCacheUnitPrice'),
            value: value.image.cacheReadPriceDisplay,
            mono: true,
          },
        ]
      : []),
    {
      label: value.image
        ? t('usage.request.textInput')
        : t('usage.request.input'),
      value: value.inputAmountDisplay,
      mono: true,
    },
    {
      label: value.image
        ? t('usage.request.imageOutput')
        : t('usage.metrics.output'),
      value: value.outputAmountDisplay,
      mono: true,
    },
    {
      label: value.image
        ? t('usage.request.textCacheRead')
        : t('usage.request.cacheRead'),
      value: value.cacheReadAmountDisplay,
      mono: true,
    },
    {
      label: t('usage.request.cacheWrites'),
      value: value.cacheWriteAmountDisplay,
      mono: true,
    },
    {
      label: t('usage.billing.standardFee'),
      value: value.standardAmountDisplay,
      mono: true,
    },
    {
      label: value.image
        ? t('usage.billing.textInputUnitPrice')
        : t('usage.billing.inputUnitPrice'),
      value: value.inputPriceDisplay,
      mono: true,
    },
    {
      label: value.image
        ? t('usage.billing.imageOutputUnitPrice')
        : t('usage.billing.outputUnitPrice'),
      value: value.outputPriceDisplay,
      mono: true,
    },
    {
      label: value.image
        ? t('usage.request.textCacheUnitPrice')
        : t('usage.request.cacheUnitPrice'),
      value: value.cacheReadPriceDisplay,
      mono: true,
    },
    {
      label: t('usage.billing.cacheWriteUnitPrice'),
      value: value.cacheWritePriceDisplay,
      mono: true,
    },
    {
      label: t('usage.billing.serviceLevel'),
      value: value.serviceTierDisplay,
    },
    {
      label: t('settings.pricing.multiplier'),
      value: value.multiplierDisplay,
      mono: true,
    },
  ]
})

function tokenCount(value: number | null | undefined): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
    ? value
    : null
}

function remainingTokenCount(total: number | null | undefined, ...parts: (number | null | undefined)[]): number | null {
  const knownTotal = tokenCount(total)
  if (knownTotal === null)
    return null

  let remainder = knownTotal
  for (const part of parts) {
    const knownPart = tokenCount(part)
    if (knownPart === null)
      return null
    remainder -= knownPart
  }
  // 未上报或子项越界时不能补零，否则环图会伪造完整的占比
  return remainder >= 0
    ? remainder
    : null
}

const tokenChartItems = computed(() => {
  const details = tokenDetails.value
  const freshInputTokens = remainingTokenCount(details?.inputTokens, details?.cachedInputTokens, details?.cacheWriteInputTokens)
  const nonReasoningOutputTokens = remainingTokenCount(details?.outputTokens, details?.reasoningOutputTokens)
  return [
    {
      label: t('usage.metrics.newInput'),
      value: freshInputTokens,
      display: freshInputTokens === null
        ? '—'
        : formatInteger(freshInputTokens),
      color: palette.value.info,
    },
    {
      label: t('usage.request.nonInferentialOutput'),
      value: nonReasoningOutputTokens,
      display: nonReasoningOutputTokens === null
        ? '—'
        : formatInteger(nonReasoningOutputTokens),
      color: palette.value.success,
    },
    {
      label: t('usage.request.cacheRead'),
      value: tokenCount(details?.cachedInputTokens),
      display: details?.cachedInputTokensDisplay ?? '—',
      color: palette.value.warning,
    },
    {
      label: t('usage.request.cacheWrites'),
      value: tokenCount(details?.cacheWriteInputTokens),
      display: details?.cacheWriteInputTokensDisplay ?? '—',
      color: palette.value.danger,
    },
    {
      label: t('usage.metrics.reasoning'),
      value: tokenCount(details?.reasoningOutputTokens),
      display: details?.reasoningOutputTokensDisplay ?? '—',
      color: palette.value.reasoning,
    },
  ]
})
const tokenChartComplete = computed(() => tokenChartItems.value.every(item => item.value !== null))
const tokenDetailItems = computed(() => [
  ...tokenChartItems.value,
  {
    label: t('usage.request.totalOutput'),
    display: tokenDetails.value?.outputTokensDisplay ?? '—',
    color: null,
  },
])

const tokenDonutOption = computed<EChartsOption>(() => {
  const items = tokenChartComplete.value
    ? tokenChartItems.value.flatMap(item => item.value !== null && item.value > 0
        ? [
            { ...item, value: item.value },
          ]
        : [])
    : []

  return {
    tooltip: {
      trigger: 'item',
      ...chartTooltipStyle(palette.value, {
        padding: [9, 12],
      }),
      formatter: (params: unknown) => {
        if (typeof params !== 'object' || params === null)
          return ''
        const name = 'name' in params && typeof params.name === 'string'
          ? params.name
          : ''
        const marker = 'marker' in params && typeof params.marker === 'string'
          ? params.marker
          : ''
        const item = tokenChartItems.value.find(entry => entry.label === name)
        if (!item)
          return ''
        return `${marker}${item.label}: ${item.display}`
      },
    },
    series: [
      {
        type: 'pie',
        radius: ['62%', '78%'],
        center: ['50%', '50%'],
        startAngle: 90,
        minAngle: items.length ? 3 : 360,
        avoidLabelOverlap: true,
        silent: !items.length,
        label: { show: false },
        labelLine: { show: false },
        data: items.length
          ? items.map(item => ({
              name: item.label,
              value: item.value,
              itemStyle: { color: item.color },
            }))
          : [
              {
                name: t('usage.request.noneYet'),
                value: 1,
                itemStyle: { color: palette.value.surfaceMuted },
              },
            ],
        emphasis: { scale: false },
      },
    ],
  }
})
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('usage.request.usageRecordDetails')"
    :description="t('usage.request.description')"
    type="info"
    width="64rem"
  >
    <div v-if="record" class="grid min-w-0 gap-3">
      <section class="grid min-w-0 gap-3 lg:grid-cols-2">
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.request.requestInformation') }}
          </h3>
          <UsageDetailFieldGrid :items="requestItems" />
        </section>
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.request.transportAndUsage') }}
          </h3>
          <UsageDetailFieldGrid :items="overviewItems" />
        </section>
      </section>

      <section class="grid min-w-0 gap-3 lg:grid-cols-2">
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.request.modelsAndRouting') }}
          </h3>
          <UsageDetailFieldGrid :items="modelRouteItems" />
        </section>

        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.request.clientAndUpstream') }}
          </h3>
          <UsageDetailFieldGrid :items="clientUpstreamItems" />
        </section>
      </section>

      <section :class="panelClass">
        <h3 :class="panelTitleClass">
          {{ t('usage.request.requestId') }}
        </h3>
        <UsageDetailFieldGrid :items="identifierItems" />
      </section>

      <section class="grid min-w-0 gap-3 lg:grid-cols-2">
        <section class="flex min-h-0 flex-col" :class="panelClass">
          <div class="flex items-center justify-between gap-2">
            <h3 :class="panelTitleClass">
              {{ t('common.units.tokens') }}
            </h3>
            <span v-if="!tokenChartComplete" class="text-cp-xs text-cp-text-tertiary">{{ t('usage.request.incompleteDetails') }}</span>
          </div>
          <div
            class="mt-3 grid min-h-38 min-w-0 flex-1 grid-cols-1 content-center items-center gap-3 sm:grid-cols-[150px_minmax(0,1fr)]"
          >
            <div class="relative mx-auto w-38 sm:mx-0">
              <BaseChart :option="tokenDonutOption" :height="152" />
              <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
                <div class="grid text-center">
                  <span class="text-cp-xs leading-none font-bold text-cp-text-quaternary">
                    {{ t('usage.request.total') }}
                  </span>
                  <strong
                    class="mt-1 font-mono text-[16px] leading-none font-extrabold tabular-nums text-cp-text"
                  >
                    {{ tokenDetails?.totalTokensDisplay ?? '—' }}
                  </strong>
                </div>
              </div>
            </div>

            <dl class="grid min-w-0 grid-cols-2 gap-x-4 gap-y-3">
              <div v-for="item in tokenDetailItems" :key="item.label" class="min-w-0">
                <dt class="flex min-w-0 items-center gap-1.5" :class="fieldLabelClass">
                  <i
                    v-if="item.color"
                    class="size-1.75 shrink-0 rounded-full"
                    :style="{ backgroundColor: item.color }"
                  />
                  <span class="truncate">{{ item.label }}</span>
                </dt>
                <dd
                  class="font-mono tabular-nums" :class="[fieldValueBaseClass]"
                  :title="displayValue(item.display)"
                >
                  {{ displayValue(item.display) }}
                </dd>
              </div>
            </dl>
          </div>
        </section>

        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.fields.cost') }}
          </h3>
          <UsageDetailFieldGrid :items="billingItems" />
        </section>
      </section>

      <section v-if="attemptRows.length" :class="panelClass">
        <h3 :class="panelTitleClass">
          {{ t('usage.error.attempts') }}
        </h3>
        <ZTable
          class="attempt-table mt-2.5 h-auto! min-w-0 font-mono tabular-nums"
          :columns="attemptColumns"
          :data="attemptRows"
          size="small"
          row-key="id"
        >
          <template #outcome="{ row }">
            <span :class="attemptOutcomeClass(row.outcome)">
              {{ attemptOutcomeText(row.outcome) }}
            </span>
          </template>
          <template #statusCode="{ row }">
            <UsageStatusCodeBadge
              :status-code="typeof row.statusCode === 'number' ? row.statusCode : null"
            />
          </template>
          <template #accountLabel="{ row }">
            <span
              class="block max-w-full truncate font-mono text-cp-sm font-bold text-cp-text"
              :title="row.accountId || ''"
            >
              {{ row.accountLabel }}
            </span>
          </template>
          <template #latencyMs="{ row }">
            <span class="font-mono font-bold tabular-nums text-cp-text">
              {{ formatDuration(row.latencyMs) }}
            </span>
          </template>
        </ZTable>
      </section>

      <section
        v-if="requestText || responseText"
        class="grid min-h-0 grid-cols-1 gap-3 lg:grid-cols-2"
      >
        <div v-if="requestText" class="min-h-0" :class="[panelClass]">
          <UsageDetailCodePanel :title="t('usage.request.requestContent')" max-height="180px" :content="requestText" />
        </div>

        <div v-if="responseText" class="min-h-0" :class="[panelClass]">
          <UsageDetailCodePanel :title="t('usage.error.responseContent')" max-height="180px" :content="responseText" />
        </div>
      </section>

      <section v-if="record.metadata" class="min-h-0" :class="[panelClass]">
        <UsageDetailCodePanel
          :title="t('usage.error.metadata')"
          max-height="min(32dvh, 340px)"
          :content="JSON.stringify(record.metadata, null, 2)"
        />
      </section>
    </div>

    <template #footer>
      <DiagnosticsDownloadButton v-if="record" :request-id="record.requestId" :active="open" />
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
</template>

<style scoped>
.attempt-table :deep(thead th) {
  background-color: transparent;
  box-shadow: none;
}

.attempt-table :deep(tbody tr) {
  background-color: transparent;
}

.attempt-table :deep(tbody td) {
  background-color: var(--cp-color-fill-tertiary);
}
</style>
