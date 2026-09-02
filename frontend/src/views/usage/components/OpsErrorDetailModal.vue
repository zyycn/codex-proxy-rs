<script setup lang="ts">
import type { OpsError } from '@/api'

import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { failureClassText } from '../utils/opsErrorPresentation'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'

interface DetailField {
  label: string
  value: unknown
  mono?: boolean
  wrap?: boolean
  fullWidth?: boolean
}

const props = defineProps<{
  record: OpsError | null
}>()

const open = defineModel<boolean>({ default: false })

const panelClass = 'min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'

const accountLabel = computed(() => props.record?.accountEmail
  || props.record?.accountName
  || props.record?.metadata.accountLabel
  || props.record?.accountId)

const errorFields = computed(() => visibleFields([
  { label: 'Provider 错误码', value: props.record?.providerErrorCode, mono: true },
  { label: '失败分类', value: failureClassText(props.record?.failureClass) },
  { label: '分类代码', value: props.record?.failureClass, mono: true },
  { label: '发送状态', value: props.record?.upstreamSendState, mono: true },
  { label: '客户端状态', value: props.record?.clientStatusCode, mono: true },
  { label: '上游状态', value: props.record?.upstreamStatusCode, mono: true },
  { label: '耗时（ms）', value: props.record?.latencyMs, mono: true },
]))

const requestFields = computed(() => visibleFields([
  { label: '账号', value: accountLabel.value, mono: true },
  { label: '时间', value: props.record?.createdAtDisplay, mono: true },
  { label: '请求 ID', value: props.record?.requestId, mono: true },
  { label: '客户端 Key ID', value: props.record?.clientApiKeyId, mono: true },
  { label: '客户端 IP', value: props.record?.clientIp, mono: true },
  { label: '协议', value: props.record?.protocol, mono: true },
  { label: 'User-Agent', value: props.record?.userAgent, mono: true, wrap: true, fullWidth: true },
]))

const routeFields = computed(() => visibleFields([
  { label: '端点', value: props.record?.route, mono: true },
  { label: 'Provider', value: props.record?.provider, mono: true },
  { label: '认证类型', value: props.record?.authenticationKind, mono: true },
  { label: '账号 ID', value: props.record?.accountId, mono: true },
  { label: '请求模型', value: props.record?.requestedModel, mono: true },
  { label: '上游模型', value: props.record?.upstreamModel, mono: true },
  { label: '记录模型', value: props.record?.model, mono: true },
  { label: '推理强度', value: props.record?.reasoningEffort, mono: true },
  { label: '推理预设', value: props.record?.reasoningPreset, mono: true },
  { label: '客户端传输', value: props.record?.clientTransport, mono: true },
  { label: '上游传输', value: props.record?.transport, mono: true },
  { label: '服务档位', value: props.record?.serviceTier, mono: true },
  { label: '请求类型', value: props.record?.requestKind, mono: true },
  { label: '子代理类型', value: props.record?.subagentKind, mono: true },
  { label: '压缩请求', value: props.record?.compact, mono: true },
]))

const eventFields = computed(() => visibleFields([
  { label: '记录来源', value: props.record?.metadata.source, mono: true },
  { label: '组件', value: props.record?.metadata.component || props.record?.kind, mono: true },
  { label: '事件类型', value: props.record?.kind, mono: true },
  { label: '操作', value: props.record?.operation, mono: true },
  { label: '尝试序号', value: props.record?.attemptIndex },
  { label: '聚合次数', value: props.record?.occurrenceCount },
  { label: '响应 ID', value: props.record?.responseId, mono: true },
  { label: '上游请求 ID', value: props.record?.upstreamRequestId, mono: true },
]))

const continuationFields = computed(() => visibleFields([
  {
    label: '会话关联 Hash',
    value: props.record?.metadata.continuationAffinityHash,
    mono: true,
    wrap: true,
    fullWidth: true,
  },
  {
    label: 'Previous Response Hash',
    value: props.record?.metadata.continuationPreviousResponseIdHash,
    mono: true,
    wrap: true,
    fullWidth: true,
  },
  { label: '续接不可用原因', value: props.record?.metadata.continuationUnavailableReason, mono: true },
  { label: '上游连接 ID', value: props.record?.metadata.upstreamConnectionId, mono: true },
  { label: '连接退出原因', value: props.record?.metadata.upstreamConnectionExitReason, mono: true },
  { label: '连接存活（ms）', value: props.record?.metadata.upstreamConnectionAgeMs, mono: true },
  { label: '最后空闲（ms）', value: props.record?.metadata.upstreamConnectionIdleMs, mono: true },
  { label: '恢复请求 ID', value: props.record?.metadata.recoveryRequestId, mono: true },
  { label: '恢复时间', value: props.record?.metadata.recoveredAt, mono: true },
  {
    label: '恢复尝试次数',
    value: props.record?.metadata.recoveryAttemptCount
      ? props.record.metadata.recoveryAttemptCount
      : null,
    mono: true,
  },
  { label: '客户端重连（ms）', value: props.record?.metadata.recoveryRetryDelayMs, mono: true },
  { label: '恢复总延迟（ms）', value: props.record?.metadata.recoveryTotalLatencyMs, mono: true },
]))

function visibleFields(items: DetailField[]) {
  return items.filter(({ value }) => value !== null && value !== undefined && value !== '')
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="错误明细"
    description="上游错误与请求上下文"
    tone="danger"
    size="xl"
  >
    <template v-if="record">
      <section :class="panelClass">
        <h3 :class="panelTitleClass">
          错误
        </h3>
        <UsageDetailFieldGrid :items="errorFields" />
        <div v-if="record.message" class="mt-3">
          <UsageDetailCodePanel title="错误信息" max-height="220px" :content="record.message" />
        </div>
      </section>

      <section v-if="record.rawUpstreamError" class="mt-3" :class="panelClass">
        <UsageDetailCodePanel
          title="上游返回原文"
          max-height="360px"
          :content="record.rawUpstreamError"
        />
      </section>

      <div class="mt-3 grid min-w-0 gap-3 lg:grid-cols-2">
        <section v-if="requestFields.length" :class="panelClass">
          <h3 :class="panelTitleClass">
            请求与客户端
          </h3>
          <UsageDetailFieldGrid :items="requestFields" />
        </section>

        <section v-if="routeFields.length" :class="panelClass">
          <h3 :class="panelTitleClass">
            路由与模型
          </h3>
          <UsageDetailFieldGrid :items="routeFields" />
        </section>
      </div>

      <section v-if="eventFields.length" class="mt-3" :class="panelClass">
        <h3 :class="panelTitleClass">
          事件记录
        </h3>
        <UsageDetailFieldGrid :items="eventFields" />
      </section>

      <section v-if="continuationFields.length" class="mt-3" :class="panelClass">
        <h3 :class="panelTitleClass">
          会话续接与物理连接
        </h3>
        <UsageDetailFieldGrid :items="continuationFields" />
      </section>
    </template>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
</template>
