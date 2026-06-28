<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import { storeToRefs } from 'pinia'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useUiStore } from '@/stores/modules/ui'
import {
  usageAccountText,
  usageClientIp,
  usageCostDetails,
  usageCostText,
  usageModelDisplay,
  usageReasoningEffort,
  usageRecordType,
  usageTokenDetails,
  usageUserAgent,
  visibleRequestText,
  visibleResponseText,
} from '../constants'
import UsageLevelBadge from './UsageLevelBadge.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const open = defineModel<boolean>({ default: false })

const props = defineProps<{
  record: any
}>()

const { themeRevision } = storeToRefs(useUiStore())

const requestText = computed(() => visibleRequestText(props.record))
const responseText = computed(() => visibleResponseText(props.record))
const modelDisplay = computed(() => usageModelDisplay(props.record))
const tokenDetails = computed(() => usageTokenDetails(props.record))
const costDetails = computed(() => usageCostDetails(props.record))

const identityItems = computed(() => [
  { label: '账号', value: usageAccountText(props.record), mono: true },
  { label: '时间', value: props.record?.createdAtDisplay, mono: true },
])

const metricItems = computed(() => [
  { label: '类型', value: usageRecordType(props.record) },
  { label: '耗时', value: props.record?.latencyMsDisplay, mono: true },
  { label: '首 Token', value: props.record?.firstTokenLatencyMsDisplay, mono: true },
  { label: '总 Token', value: tokenDetails.value.totalTokensDisplay, mono: true },
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
      { label: '推理强度', value: usageReasoningEffort(props.record) },
      { label: '账号 ID', value: props.record?.accountId, mono: true },
      { label: '请求 ID', value: props.record?.requestId, mono: true },
      { label: '响应 ID', value: props.record?.responseId, mono: true },
      { label: '上游请求 ID', value: props.record?.upstreamRequestId, mono: true },
    ],
  },
  {
    title: '客户端与上游',
    items: [
      { label: '客户端 IP', value: usageClientIp(props.record), mono: true },
      { label: 'User-Agent', value: props.record ? usageUserAgent(props.record) : '', mono: true },
      { label: '传输方式', value: props.record?.transport, mono: true },
      { label: '事件类型', value: props.record?.kind, mono: true },
      { label: '尝试序号', value: props.record?.attemptIndex },
      { label: '上游状态码', value: props.record?.upstreamStatusCode },
      { label: '失败分类', value: props.record?.failureClass, mono: true },
    ],
  },
])

const costItems = computed(() => {
  const details = costDetails.value
  if (!details) {
    return [{ label: '总费用', value: usageCostText(props.record), mono: true }]
  }

  return [
    { label: '总费用', value: details.totalCostDisplay, mono: true },
    { label: '输入', value: details.inputCostDisplay, mono: true },
    { label: '输出', value: details.outputCostDisplay, mono: true },
    { label: '缓存读取', value: details.cacheReadCostDisplay, mono: true },
    { label: '计费', value: details.billedCostDisplay, mono: true },
    { label: '原始', value: details.originalCostDisplay, mono: true },
    { label: '输入单价', value: details.inputPriceDisplay, mono: true },
    { label: '输出单价', value: details.outputPriceDisplay, mono: true },
    { label: '缓存单价', value: details.cacheReadPriceDisplay, mono: true },
    { label: '服务层级', value: details.serviceTierDisplay },
    { label: '倍率', value: details.multiplierDisplay, mono: true },
  ]
})

const tokenChartItems = computed(() => [
  {
    label: '输入',
    value: Number(tokenDetails.value.inputTokens || 0),
    display: tokenDetails.value.inputTokensDisplay,
    color: themeColor('--cp-info', '#2563EB'),
  },
  {
    label: '输出',
    value: Number(tokenDetails.value.outputTokens || 0),
    display: tokenDetails.value.outputTokensDisplay,
    color: themeColor('--cp-success', '#10B981'),
  },
  {
    label: '缓存读取',
    value: Number(tokenDetails.value.cachedTokens || 0),
    display: tokenDetails.value.cachedTokensDisplay,
    color: themeColor('--cp-warning', '#D97706'),
  },
  {
    label: '推理',
    value: Number(tokenDetails.value.reasoningTokens || 0),
    display: tokenDetails.value.reasoningTokensDisplay,
    color: themeColor('--cp-focus-ring', '#8B5CF6'),
  },
])

const tokenDonutOption = computed<EChartsOption>(() => {
  const items = tokenChartItems.value.filter((item) => item.value > 0)

  return {
    tooltip: {
      trigger: 'item',
      backgroundColor: themeColor('--cp-bg-surface', '#fff'),
      borderColor: 'transparent',
      borderWidth: 0,
      padding: [9, 12],
      textStyle: {
        color: themeColor('--cp-text-primary', '#334155'),
        fontSize: 12,
        fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
        fontWeight: 650,
      },
      extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
      formatter: (params: any) => {
        const item = tokenChartItems.value.find((entry) => entry.label === params.name)
        if (!item) return ''
        return `${params.marker}${item.label}: ${item.display}`
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
          ? items.map((item) => ({
              name: item.label,
              value: item.value,
              itemStyle: { color: item.color },
            }))
          : [
              {
                name: '暂无',
                value: 1,
                itemStyle: { color: themeColor('--cp-bg-muted', '#E5E7EB') },
              },
            ],
        emphasis: { scale: false },
      },
    ],
  }
})

function displayValue(value: unknown) {
  if (value === undefined || value === null || value === '') return '—'
  return String(value)
}

function themeColor(name: string, fallback: string) {
  themeRevision.value
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="使用记录详情"
    description="查看单次网关请求的模型、链路、Token、费用与上游响应。"
    variant="info"
    width="920px"
  >
    <div v-if="record" class="flex max-h-[min(76vh,820px)] flex-col overflow-hidden">
      <BaseScrollbar view-class="grid gap-4 pr-3" max-height="min(76vh,820px)">
        <section class="grid gap-3 lg:grid-cols-2">
          <section class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3.5 py-3">
            <h3 class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
              请求身份
            </h3>
            <dl class="mt-3 grid grid-cols-1 gap-x-4 gap-y-2.5 sm:grid-cols-[minmax(0,1fr)_180px]">
              <div v-for="item in identityItems" :key="item.label" class="min-w-0">
                <dt class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                  {{ item.label }}
                </dt>
                <dd
                  class="mt-1.5 mb-0 overflow-hidden text-ellipsis whitespace-nowrap text-[12px] leading-none font-[700] text-(--cp-text-primary)"
                  :class="item.mono ? 'font-mono tabular-nums' : undefined"
                  :title="displayValue(item.value)"
                >
                  {{ displayValue(item.value) }}
                </dd>
              </div>
            </dl>
          </section>

          <section class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3.5 py-3">
            <h3 class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
              执行指标
            </h3>
            <dl class="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
              <div v-for="item in metricItems" :key="item.label" class="min-w-0">
                <dt class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                  {{ item.label }}
                </dt>
                <dd
                  class="mt-1.5 mb-0 truncate text-[12px] leading-none font-[700] text-(--cp-text-primary)"
                  :class="item.mono ? 'font-mono tabular-nums' : undefined"
                  :title="displayValue(item.value)"
                >
                  {{ displayValue(item.value) }}
                </dd>
              </div>
            </dl>
          </section>
        </section>

        <section class="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <div class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5">
            <span class="block text-[11px] leading-none font-bold text-(--cp-text-muted)">
              结果
            </span>
            <div class="mt-1.5">
              <UsageLevelBadge :level="record.level" />
            </div>
          </div>
          <div class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5">
            <span class="block text-[11px] leading-none font-bold text-(--cp-text-muted)">
              状态码
            </span>
            <div class="mt-1.5">
              <UsageStatusCodeBadge :status-code="record.statusCode" />
            </div>
          </div>
          <div
            class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 lg:col-span-2"
          >
            <span class="block text-[11px] leading-none font-bold text-(--cp-text-muted)">
              消息
            </span>
            <p class="mt-2 mb-0 truncate text-[13px] font-[650] text-(--cp-text-primary)">
              {{ displayValue(record.message) }}
            </p>
          </div>
        </section>

        <section class="grid gap-3 lg:grid-cols-2">
          <section
            v-for="group in detailGroups"
            :key="group.title"
            class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3.5 py-3"
          >
            <h3 class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
              {{ group.title }}
            </h3>
            <dl class="mt-3 grid grid-cols-1 gap-x-4 gap-y-2.5 sm:grid-cols-2">
              <div v-for="item in group.items" :key="item.label" class="min-w-0">
                <dt class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                  {{ item.label }}
                </dt>
                <dd
                  class="mt-1.5 mb-0 truncate text-[12px] leading-none font-[700] text-(--cp-text-primary)"
                  :class="item.mono ? 'font-mono tabular-nums' : undefined"
                  :title="displayValue(item.value)"
                >
                  {{ displayValue(item.value) }}
                </dd>
              </div>
            </dl>
          </section>
        </section>

        <section class="grid gap-3 lg:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)]">
          <section
            class="flex flex-col rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3.5 py-3"
          >
            <h3 class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
              Token
            </h3>
            <div
              class="mt-3 grid min-h-38 flex-1 grid-cols-1 content-center items-center gap-3 sm:grid-cols-[150px_minmax(0,1fr)]"
            >
              <div class="relative mx-auto w-38 sm:mx-0">
                <BaseChart :option="tokenDonutOption" :height="152" />
                <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
                  <div class="grid text-center">
                    <span class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                      总计
                    </span>
                    <strong
                      class="mt-1 font-mono text-[16px] leading-none font-extrabold tabular-nums text-(--cp-text-primary)"
                    >
                      {{ tokenDetails.totalTokensDisplay }}
                    </strong>
                  </div>
                </div>
              </div>

              <dl class="grid grid-cols-2 gap-x-4 gap-y-2.5">
                <div v-for="item in tokenChartItems" :key="item.label" class="min-w-0">
                  <dt
                    class="flex min-w-0 items-center gap-1.5 text-[11px] leading-none font-bold text-(--cp-text-muted)"
                  >
                    <i
                      class="size-1.75 shrink-0 rounded-full"
                      :style="{ backgroundColor: item.color }"
                    />
                    <span class="truncate">{{ item.label }}</span>
                  </dt>
                  <dd
                    class="mt-1.5 mb-0 truncate font-mono text-[12px] leading-none font-[700] tabular-nums text-(--cp-text-primary)"
                    :title="displayValue(item.display)"
                  >
                    {{ displayValue(item.display) }}
                  </dd>
                </div>
              </dl>
            </div>
          </section>

          <section class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3.5 py-3">
            <h3 class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">费用</h3>
            <dl class="mt-3 grid grid-cols-1 gap-x-4 gap-y-2.5 sm:grid-cols-2">
              <div v-for="item in costItems" :key="item.label" class="min-w-0">
                <dt class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
                  {{ item.label }}
                </dt>
                <dd
                  class="mt-1.5 mb-0 truncate text-[12px] leading-none font-[700] text-(--cp-text-primary)"
                  :class="item.mono ? 'font-mono tabular-nums' : undefined"
                  :title="displayValue(item.value)"
                >
                  {{ displayValue(item.value) }}
                </dd>
              </div>
            </dl>
          </section>
        </section>

        <section
          v-if="requestText || responseText"
          class="grid min-h-0 grid-cols-1 gap-3 lg:grid-cols-2"
        >
          <div v-if="requestText" class="min-h-0">
            <span class="mb-1 block text-[11px] font-bold text-(--cp-text-muted)">请求内容</span>
            <BaseScrollbar
              max-height="180px"
              view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5"
            >
              <pre
                class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] text-(--cp-text-primary)"
                >{{ requestText }}</pre
              >
            </BaseScrollbar>
          </div>

          <div v-if="responseText" class="min-h-0">
            <span class="mb-1 block text-[11px] font-bold text-(--cp-text-muted)">响应内容</span>
            <BaseScrollbar
              max-height="180px"
              view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5"
            >
              <pre
                class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] text-(--cp-text-primary)"
                >{{ responseText }}</pre
              >
            </BaseScrollbar>
          </div>
        </section>

        <section v-if="record.metadata" class="min-h-0">
          <span class="mb-1 block text-[11px] font-bold text-(--cp-text-muted)">元数据</span>
          <BaseScrollbar
            max-height="min(32vh, 340px)"
            view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5"
          >
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.65] text-(--cp-text-primary)"
              >{{ JSON.stringify(record.metadata, null, 2) }}</pre
            >
          </BaseScrollbar>
        </section>
      </BaseScrollbar>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">关闭</BaseButton>
    </template>
  </BaseModal>
</template>
