<script setup lang="ts">
import type { OpsError } from '@/api'

import { computed } from 'vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { displayValue } from '../utils/detail'
import { presentOpsError } from '../utils/opsErrorPresentation'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  record: OpsError | null
}>()

const open = defineModel<boolean>({ default: false })

const presentation = computed(() => props.record ? presentOpsError(props.record) : null)

const panelClass = 'min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'

const accountLabel = computed(() => props.record?.accountEmail
  || props.record?.accountName
  || props.record?.metadata.accountLabel
  || props.record?.accountId)

const requestFields = computed(() => [
  { label: '客户端 IP', value: props.record?.clientIp, mono: true },
  { label: 'User-Agent', value: props.record?.userAgent, mono: true, wrap: true },
  { label: '协议', value: props.record?.protocol, mono: true },
  { label: '客户端传输', value: props.record?.clientTransport, mono: true },
  { label: '客户端 Key ID', value: props.record?.clientApiKeyId, mono: true },
  { label: '请求 ID', value: props.record?.requestId, mono: true },
  { label: '响应 ID', value: props.record?.responseId, mono: true },
  { label: '上游请求 ID', value: props.record?.upstreamRequestId, mono: true },
])

const routeFields = computed(() => [
  { label: '端点', value: props.record?.route, mono: true },
  { label: '请求模型', value: props.record?.requestedModel, mono: true },
  { label: '上游模型', value: props.record?.upstreamModel, mono: true },
  { label: '记录模型', value: props.record?.model, mono: true },
  { label: '推理强度', value: props.record?.reasoningEffort, mono: true },
  { label: '推理预设', value: props.record?.reasoningPreset, mono: true },
  { label: '请求类型', value: props.record?.requestKind, mono: true },
  { label: '子代理类型', value: props.record?.subagentKind, mono: true },
  { label: '压缩请求', value: booleanDisplay(props.record?.compact) },
  { label: '服务档位', value: props.record?.serviceTier, mono: true },
  { label: '上游传输', value: props.record?.transport, mono: true },
  { label: '尝试序号', value: props.record?.attemptIndex },
])

const diagnosticFields = computed(() => [
  { label: '账号', value: accountLabel.value, mono: true },
  { label: '时间', value: props.record?.createdAtDisplay, mono: true },
  { label: '记录来源', value: presentation.value?.sourceLabel },
  { label: '组件', value: presentation.value?.componentLabel },
  { label: '操作/触发', value: props.record?.operation, mono: true },
  { label: '失败分类', value: presentation.value?.failureClassLabel },
  { label: '平台/类型', value: providerKindLabel(props.record) },
  { label: '账号 ID', value: props.record?.accountId, mono: true },
  { label: 'Provider 错误码', value: props.record?.providerErrorCode, mono: true },
  { label: '聚合次数', value: props.record?.occurrenceCount },
  { label: '耗时', value: latencyDisplay(props.record?.latencyMs), mono: true },
])

const metadataText = computed(() => {
  const metadata = props.record?.metadata
  if (!metadata || (typeof metadata === 'object' && Object.keys(metadata).length === 0))
    return ''
  return JSON.stringify(metadata, null, 2)
})

function latencyDisplay(value: unknown) {
  return typeof value === 'number' ? `${value} ms` : '—'
}

function booleanDisplay(value: boolean | null | undefined) {
  if (value === true)
    return '是'
  if (value === false)
    return '否'
  return '—'
}

function providerKindLabel(record: OpsError | null) {
  if (!record?.provider)
    return '—'
  return record.authenticationKind
    ? `${record.provider} · ${record.authenticationKind}`
    : record.provider
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="错误明细"
    description="请求来源、链路上下文与原始诊断"
    tone="danger"
    size="xl"
  >
    <template v-if="record">
      <section class="rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5">
        <div class="flex flex-wrap items-center gap-3">
          <UsageStatusCodeBadge :status-code="record.clientStatusCode" />
          <span class="text-cp-sm font-bold text-cp-text-secondary">
            客户端 {{ displayValue(record.clientStatusCode) }}
          </span>
          <span class="text-cp-sm font-bold text-cp-text-secondary">
            上游 {{ displayValue(record.upstreamStatusCode) }}
          </span>
        </div>
        <p class="mt-3 mb-0 text-cp leading-relaxed font-bold text-cp-text">
          {{ displayValue(presentation?.summary) }}
        </p>
      </section>

      <div class="mt-3 grid min-w-0 gap-3 lg:grid-cols-2">
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            请求追踪
          </h3>
          <UsageDetailFieldGrid :items="requestFields" />
        </section>

        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            模型与链路
          </h3>
          <UsageDetailFieldGrid :items="routeFields" />
        </section>
      </div>

      <section class="mt-3" :class="panelClass">
        <h3 :class="panelTitleClass">
          错误事件
        </h3>
        <UsageDetailFieldGrid :items="diagnosticFields" />
      </section>

      <section class="mt-3" :class="panelClass">
        <UsageDetailCodePanel title="原始诊断" max-height="260px" :content="record.message" />
      </section>

      <section
        v-if="metadataText"
        class="mt-3"
        :class="panelClass"
      >
        <UsageDetailCodePanel title="Metadata" max-height="260px" :content="metadataText" />
      </section>
    </template>
  </BaseModal>
</template>
