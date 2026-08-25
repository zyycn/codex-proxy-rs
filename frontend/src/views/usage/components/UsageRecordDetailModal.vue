<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { UsageViewModel } from '../utils/records'

import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { displayValue, fieldLabelClass, fieldValueBaseClass, fieldValueClass } from '../utils/detail'
import { formatDuration } from '../utils/format'
import {
  usageAccountText,
  usageBilling,
  usageBillingText,
  usageClientIp,
  usageLatencyDetails,
  usageModelDisplay,
  usageReasoningEffort,
  usageRecordType,
  usageTokenDetails,
  usageUserAgent,
  visibleRequestText,
  visibleResponseText,
} from '../utils/records'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  record: UsageViewModel | null
}>()

const open = defineModel<boolean>({ default: false })

const { palette } = useChartPalette()

const requestText = computed(() => props.record ? visibleRequestText(props.record) : '')
const responseText = computed(() => props.record ? visibleResponseText(props.record) : '')
const modelDisplay = computed(() => props.record
  ? usageModelDisplay(props.record)
  : { primary: '—', secondary: '' })
const tokenDetails = computed(() => props.record ? usageTokenDetails(props.record) : null)
const billing = computed(() => props.record ? usageBilling(props.record) : null)
const latencyDetails = computed(() => props.record ? usageLatencyDetails(props.record) : null)

const panelClass = 'rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'

const accountDisplay = computed(() => props.record ? usageAccountText(props.record) : '—')
const finalAttemptIndex = computed(() => {
  const attempts = props.record?.attempts ?? []
  const last = attempts[attempts.length - 1]
  return last?.attemptIndex ?? props.record?.attemptCount
})
const overviewItems = computed(() => [
  { label: '时间', value: props.record?.createdAtDisplay, mono: true },
  { label: '类型', value: props.record ? usageRecordType(props.record) : '—' },
  { label: '耗时', value: props.record?.latencyMsDisplay, mono: true },
  {
    label: latencyDetails.value?.firstOutputLabel ?? '首字',
    value: latencyDetails.value?.firstOutputDisplay ?? '—',
    mono: true,
  },
  { label: '总 Token', value: tokenDetails.value?.totalTokensDisplay, mono: true },
  { label: '请求 ID', value: props.record?.requestId, mono: true, wide: true },
  { label: '消息', value: props.record?.message, wide: true },
])

const detailGroups = computed(() => [
  {
    title: '模型与链路',
    items: [
      { label: '端点', value: props.record?.route, mono: true },
      { label: '请求模型', value: modelDisplay.value.primary, mono: true },
      {
        label: '上游模型',
        value: modelDisplay.value.secondary || props.record?.upstreamModel,
        mono: true,
      },
      { label: '存储模型', value: props.record?.model, mono: true },
      { label: '推理强度', value: props.record ? usageReasoningEffort(props.record) : '—' },
      { label: '账号 ID', value: props.record?.accountId, mono: true },
      { label: '请求 ID', value: props.record?.requestId, mono: true },
      { label: '响应 ID', value: props.record?.responseId, mono: true },
      { label: '上游请求 ID', value: props.record?.upstreamRequestId, mono: true },
    ],
  },
  {
    title: '客户端与上游',
    items: [
      { label: '客户端 IP', value: props.record ? usageClientIp(props.record) : '—', mono: true },
      {
        label: 'User-Agent',
        value: props.record ? usageUserAgent(props.record) : '',
        mono: true,
        wrap: true,
      },
      { label: '传输方式', value: props.record?.transport, mono: true },
      { label: '事件类型', value: props.record?.kind, mono: true },
      { label: '尝试序号', value: finalAttemptIndex.value },
      { label: '服务档位', value: props.record?.serviceTier, mono: true },
      { label: '客户端 Key ID', value: props.record?.clientApiKeyId, mono: true },
    ],
  },
])
const modelRouteGroup = computed(() => detailGroups.value[0])
const clientUpstreamGroup = computed(() => detailGroups.value[1])

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

const attemptColumns = defineTableColumns<AttemptRow>([
  { key: 'attemptIndex', label: '序号', kind: 'index' },
  { key: 'outcome', label: '结果', kind: 'status', size: 'sm' },
  { key: 'provider', label: '平台', kind: 'meta', size: 'sm' },
  { key: 'model', label: '模型', kind: 'mono', size: 'lg' },
  { key: 'transport', label: '传输', kind: 'status', size: 'sm' },
  { key: 'statusCode', label: '状态', kind: 'status', size: 'sm' },
  { key: 'accountLabel', label: '账号', kind: 'mono', size: '3xl' },
  { key: 'latencyMs', label: '耗时', kind: 'numeric', size: 'sm' },
])

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

function attemptOutcomeText(outcome: string) {
  const labels: Record<string, string> = {
    succeeded: '成功',
    failed: '失败',
    cancelled: '取消',
    incomplete: '未完成',
    running: '进行中',
  }
  return labels[outcome] ?? outcome
}

function attemptOutcomeClass(outcome: string) {
  if (outcome === 'succeeded')
    return 'text-cp-success-text'
  if (outcome === 'failed')
    return 'text-cp-error-text'
  return 'text-cp-text-secondary'
}

const billingItems = computed(() => {
  const value = billing.value
  if (!value) {
    return [{
      label: '总费用',
      value: props.record ? usageBillingText(props.record) : '—',
      mono: true,
    }]
  }

  return [
    { label: '总费用', value: value.totalAmountDisplay, mono: true },
    { label: '输入', value: value.inputAmountDisplay, mono: true },
    { label: '输出', value: value.outputAmountDisplay, mono: true },
    { label: '缓存读取', value: value.cacheReadAmountDisplay, mono: true },
    { label: '缓存写入', value: value.cacheWriteAmountDisplay, mono: true },
    { label: '标准费用', value: value.standardAmountDisplay, mono: true },
    { label: '输入单价', value: value.inputPriceDisplay, mono: true },
    { label: '输出单价', value: value.outputPriceDisplay, mono: true },
    { label: '缓存单价', value: value.cacheReadPriceDisplay, mono: true },
    { label: '缓存写入单价', value: value.cacheWritePriceDisplay, mono: true },
    { label: '服务档位', value: value.serviceTierDisplay },
    { label: '倍率', value: value.multiplierDisplay, mono: true },
  ]
})

const tokenChartItems = computed(() => [
  {
    label: '输入',
    value: Number(tokenDetails.value?.inputTokens || 0),
    display: tokenDetails.value?.inputTokensDisplay ?? '—',
    color: palette.value.info,
  },
  {
    label: '输出',
    value: Number(tokenDetails.value?.outputTokens || 0),
    display: tokenDetails.value?.outputTokensDisplay ?? '—',
    color: palette.value.success,
  },
  {
    label: '缓存读取',
    value: Number(tokenDetails.value?.cachedTokens || 0),
    display: tokenDetails.value?.cachedTokensDisplay ?? '—',
    color: palette.value.warning,
  },
  {
    label: '缓存写入',
    value: Number(tokenDetails.value?.cacheWriteTokens || 0),
    display: tokenDetails.value?.cacheWriteTokensDisplay ?? '—',
    color: palette.value.danger,
  },
  {
    label: '推理',
    value: Number(tokenDetails.value?.reasoningTokens || 0),
    display: tokenDetails.value?.reasoningTokensDisplay ?? '—',
    color: palette.value.reasoning,
  },
])

const tokenDonutOption = computed<EChartsOption>(() => {
  const items = tokenChartItems.value.filter(item => item.value > 0)

  return {
    tooltip: {
      trigger: 'item',
      ...chartTooltipStyle(palette.value, { padding: [9, 12] }),
      formatter: (params: unknown) => {
        if (typeof params !== 'object' || params === null)
          return ''
        const name = 'name' in params && typeof params.name === 'string' ? params.name : ''
        const marker = 'marker' in params && typeof params.marker === 'string' ? params.marker : ''
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
                name: '暂无',
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
  <BaseModal
    v-model="open"
    title="使用记录详情"
    description="单次请求的完整链路信息"
    tone="info"
    size="xl"
  >
    <div v-if="record" class="grid gap-3">
      <section :class="panelClass">
        <div class="min-w-0">
          <span :class="fieldLabelClass">账号</span>
          <p
            class="mt-1.5 mb-0 truncate font-mono text-cp leading-none font-heavy text-cp-text"
            :title="displayValue(accountDisplay)"
          >
            {{ displayValue(accountDisplay) }}
          </p>
        </div>

        <dl
          class="mt-4 grid grid-cols-2 gap-x-5 gap-y-3 lg:grid-cols-[120px_120px_repeat(5,minmax(0,1fr))]"
        >
          <div class="min-w-0">
            <dt :class="fieldLabelClass">
              状态码
            </dt>
            <dd class="mt-1.5 mb-0">
              <UsageStatusCodeBadge :status-code="record.statusCode" />
            </dd>
          </div>

          <div
            v-for="item in overviewItems"
            :key="item.label"
            class="min-w-0"
            :class="item.wide ? 'col-span-2 lg:col-span-2' : undefined"
          >
            <dt :class="fieldLabelClass">
              {{ item.label }}
            </dt>
            <dd :class="fieldValueClass(item.mono)" :title="displayValue(item.value)">
              {{ displayValue(item.value) }}
            </dd>
          </div>
        </dl>
      </section>

      <section class="grid gap-3 lg:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)]">
        <div class="flex min-h-0 flex-col gap-3">
          <section :class="panelClass">
            <h3 :class="panelTitleClass">
              {{ modelRouteGroup.title }}
            </h3>
            <UsageDetailFieldGrid :items="modelRouteGroup.items" />
          </section>

          <section class="flex min-h-0 flex-1 flex-col" :class="[panelClass]">
            <h3 :class="panelTitleClass">
              Token
            </h3>
            <div
              class="mt-3 grid min-h-38 flex-1 grid-cols-1 content-center items-center gap-3 sm:grid-cols-[150px_minmax(0,1fr)]"
            >
              <div class="relative mx-auto w-38 sm:mx-0">
                <BaseChart :option="tokenDonutOption" :height="152" />
                <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
                  <div class="grid text-center">
                    <span class="text-cp-xs leading-none font-bold text-cp-text-quaternary">
                      总计
                    </span>
                    <strong
                      class="mt-1 font-mono text-[16px] leading-none font-extrabold tabular-nums text-cp-text"
                    >
                      {{ tokenDetails?.totalTokensDisplay ?? '—' }}
                    </strong>
                  </div>
                </div>
              </div>

              <dl class="grid grid-cols-2 gap-x-4 gap-y-3">
                <div v-for="item in tokenChartItems" :key="item.label" class="min-w-0">
                  <dt class="flex min-w-0 items-center gap-1.5" :class="fieldLabelClass">
                    <i
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
        </div>

        <div class="flex min-h-0 flex-col gap-3">
          <section class="flex-1" :class="[panelClass]">
            <h3 :class="panelTitleClass">
              {{ clientUpstreamGroup.title }}
            </h3>
            <UsageDetailFieldGrid :items="clientUpstreamGroup.items" />
          </section>

          <section :class="panelClass">
            <h3 :class="panelTitleClass">
              费用
            </h3>
            <UsageDetailFieldGrid :items="billingItems" />
          </section>
        </div>
      </section>

      <section v-if="attemptRows.length" :class="panelClass">
        <div class="flex items-center gap-2">
          <h3 :class="panelTitleClass">
            尝试链路
          </h3>
          <span
            class="text-cp-xs leading-none font-emphasis text-cp-text-quaternary"
            title="中间尝试可能有缺口，仅展示可观测到的记录"
          >
            尽力观测
          </span>
        </div>
        <BaseTable
          class="attempt-table mt-2.5 min-w-0 font-mono tabular-nums"
          :columns="attemptColumns"
          :rows="attemptRows"
          density="compact"
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
        </BaseTable>
      </section>

      <section
        v-if="requestText || responseText"
        class="grid min-h-0 grid-cols-1 gap-3 lg:grid-cols-2"
      >
        <div v-if="requestText" class="min-h-0" :class="[panelClass]">
          <UsageDetailCodePanel title="请求内容" max-height="180px" :content="requestText" />
        </div>

        <div v-if="responseText" class="min-h-0" :class="[panelClass]">
          <UsageDetailCodePanel title="响应内容" max-height="180px" :content="responseText" />
        </div>
      </section>

      <section v-if="record.providerMetadata" class="min-h-0" :class="[panelClass]">
        <UsageDetailCodePanel
          title="元数据"
          max-height="min(32dvh, 340px)"
          :content="JSON.stringify(record.providerMetadata, null, 2)"
        />
      </section>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
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
