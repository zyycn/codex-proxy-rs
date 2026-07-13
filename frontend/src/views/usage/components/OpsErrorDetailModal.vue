<script setup lang="ts">
import { computed } from 'vue'

import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const open = defineModel<boolean>({ default: false })

const props = defineProps<{
  record: any | null
}>()

const fields = computed(() => [
  { label: '时间', value: props.record?.createdAtDisplay },
  { label: '事件', value: props.record?.kind },
  { label: '失败分类', value: props.record?.failureClass },
  { label: 'Provider', value: props.record?.provider },
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
  if (!metadata || (typeof metadata === 'object' && Object.keys(metadata).length === 0)) return ''
  return JSON.stringify(metadata, null, 2)
})

function display(value: unknown) {
  return value === undefined || value === null || value === '' ? '—' : String(value)
}

function latencyDisplay(value: unknown) {
  return typeof value === 'number' ? `${value} ms` : '—'
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="错误明细"
    description="状态码、失败分类与诊断信息"
    variant="danger"
    width="920px"
    body-max-height="min(76dvh,760px)"
  >
    <template v-if="record">
      <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5">
        <div class="flex flex-wrap items-center gap-3">
          <UsageStatusCodeBadge :status-code="record.statusCode" />
          <span class="text-[12px] font-bold text-(--cp-text-secondary)">
            客户端 {{ display(record.clientStatusCode) }}
          </span>
          <span class="text-[12px] font-bold text-(--cp-text-secondary)">
            上游 {{ display(record.upstreamStatusCode) }}
          </span>
        </div>
        <p class="mt-3 mb-0 text-[13px] leading-relaxed font-[680] text-(--cp-text-primary)">
          {{ display(record.message) }}
        </p>
      </section>

      <dl
        class="mt-3 grid grid-cols-1 gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5 sm:grid-cols-2"
      >
        <div
          v-for="field in fields"
          :key="field.label"
          class="min-w-0"
          :class="field.wide ? 'sm:col-span-2' : undefined"
        >
          <dt class="text-[11px] leading-none font-bold text-(--cp-text-muted)">
            {{ field.label }}
          </dt>
          <dd
            class="mt-1.5 mb-0 truncate font-mono text-[12px] leading-normal font-[650] text-(--cp-text-primary)"
            :title="display(field.value)"
          >
            {{ display(field.value) }}
          </dd>
        </div>
      </dl>

      <section
        v-if="metadataText"
        class="mt-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5"
      >
        <h3 class="m-0 text-[12px] leading-none font-[780] text-(--cp-text-secondary)">Metadata</h3>
        <BaseScrollbar
          class="mt-3"
          max-height="260px"
          view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) px-3 py-2.5"
        >
          <pre
            class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.65] text-(--cp-text-primary)"
            >{{ metadataText }}</pre>
        </BaseScrollbar>
      </section>
    </template>
  </BaseModal>
</template>
