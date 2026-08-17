<script setup lang="ts">
import type { getOpsErrors } from '@/api'

import { computed } from 'vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { displayValue } from '../utils/detail'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{
  record: Awaited<ReturnType<typeof getOpsErrors>>['items'][number] | null
}>()

const open = defineModel<boolean>({ default: false })

const fields = computed(() => [
  { label: '账号', value: props.record?.metadata.accountLabel },
  { label: '时间', value: props.record?.createdAtDisplay },
  { label: '事件', value: props.record?.kind },
  { label: '失败分类', value: props.record?.failureClass },
  { label: '平台/类型', value: providerKindLabel(props.record) },
  { label: '端点', value: props.record?.route },
  { label: '模型', value: props.record?.model },
  { label: '账号 ID', value: props.record?.accountId },
  { label: '客户端 Key ID', value: props.record?.clientApiKeyId },
  { label: '传输方式', value: props.record?.transport },
  { label: '尝试序号', value: props.record?.attemptIndex },
  { label: '耗时', value: latencyDisplay(props.record?.latencyMs) },
  { label: '请求 ID', value: props.record?.requestId, wide: true },
  { label: '响应 ID', value: props.record?.responseId, wide: true },
  { label: '上游请求 ID', value: props.record?.upstreamRequestId, wide: true },
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

function providerKindLabel(record: Awaited<ReturnType<typeof getOpsErrors>>['items'][number] | null) {
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
    description="状态码、失败分类与诊断信息"
    tone="danger"
    size="xl"
  >
    <template v-if="record">
      <section class="rounded-cp-surface bg-cp-subtle px-4 py-3.5">
        <div class="flex flex-wrap items-center gap-3">
          <UsageStatusCodeBadge :status-code="record.clientStatusCode" />
          <span class="text-[12px] font-bold text-cp-secondary">
            客户端 {{ displayValue(record.clientStatusCode) }}
          </span>
          <span class="text-[12px] font-bold text-cp-secondary">
            上游 {{ displayValue(record.upstreamStatusCode) }}
          </span>
        </div>
        <p class="mt-3 mb-0 text-[13px] leading-relaxed font-bold text-cp-primary">
          {{ displayValue(record.message) }}
        </p>
      </section>

      <dl
        class="mt-3 grid grid-cols-1 gap-3 rounded-cp-surface bg-cp-subtle px-4 py-3.5 sm:grid-cols-2"
      >
        <div
          v-for="field in fields"
          :key="field.label"
          class="min-w-0"
          :class="field.wide ? 'sm:col-span-2' : undefined"
        >
          <dt class="text-[11px] leading-none font-bold text-cp-muted-text">
            {{ field.label }}
          </dt>
          <dd
            class="mt-1.5 mb-0 truncate font-mono text-[12px] leading-normal font-emphasis text-cp-primary"
            :title="displayValue(field.value)"
          >
            {{ displayValue(field.value) }}
          </dd>
        </div>
      </dl>

      <section
        v-if="metadataText"
        class="mt-3 rounded-cp-surface bg-cp-subtle px-4 py-3.5"
      >
        <UsageDetailCodePanel title="Metadata" max-height="260px" :content="metadataText" />
      </section>
    </template>
  </BaseModal>
</template>
