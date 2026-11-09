<script setup lang="ts">
import type { UsageRecordDetail } from '@/api'

import { ZButton, ZDialog } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageDisplayOutcome, usageErrorSummary, usageErrorTitle } from '@/components/usage/shared/errors'
import { usageAccountText, usageModelDisplay, usageTransportType } from '@/components/usage/shared/presenter'
import { formatDuration } from '@/utils/format'
import { attemptOutcomeClass, attemptOutcomeText, visibleResponseText } from '../utils/detail'
import DiagnosticsDownloadButton from './DiagnosticsDownloadButton.vue'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageDetailState from './UsageDetailState.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{ record: UsageRecordDetail | null, loading?: boolean, error?: string }>()

const emit = defineEmits<{ retry: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })

const panelClass = 'min-w-0 rounded-z-card bg-z-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-z-sm leading-none font-heavy text-z-text-secondary'

const errorTitle = computed(() => props.record
  ? usageErrorTitle(props.record)
  : '')
const errorMessage = computed(() => {
  const message = props.record?.message.trim()
  return message && message !== props.record?.logicalOutcome
    ? message
    : t('usage.error.noDetails')
})
const errorSummary = computed(() => props.record
  ? usageErrorSummary(props.record)
  : null)
const upstreamMessage = computed(() => props.record?.upstreamError?.message)
const errorItems = computed(() => [
  { label: 'Code', value: props.record?.upstreamError?.code ?? props.record?.errorCode },
  { label: 'Type', value: props.record?.upstreamError?.errorType },
  { label: 'Param', value: props.record?.upstreamError?.param },
  {
    label: t('usage.error.websocketCloseCode'),
    value: props.record?.websocketCloseCode,
  },
  {
    label: t('usage.error.lastEventType'),
    value: props.record?.upstreamError?.lastEventType,
  },
  {
    label: t('usage.error.retryAfter'),
    value: props.record?.retryAfterMs == null
      ? null
      : formatDuration(props.record.retryAfterMs),
  },
].filter(item => item.value != null && item.value !== '').map(item => ({ ...item, mono: true, wrap: true })))
const responseText = computed(() => props.record
  ? visibleResponseText(props.record)
  : '')
const metadataText = computed(() => props.record && Object.keys(props.record.metadata).length
  ? JSON.stringify(props.record.metadata, null, 2)
  : '')
const contextItems = computed(() => [
  {
    label: t('accounts.account'),
    value: props.record
      ? usageAccountText(props.record)
      : null,
    mono: true,
    fullWidth: true,
  },
  {
    label: t('usage.fields.time'),
    value: props.record?.createdAtDisplay,
    mono: true,
  },
  {
    label: t('usage.latency.totalLatency'),
    value: props.record?.latencyMsDisplay,
    mono: true,
  },
  {
    label: t('usage.error.platform'),
    value: props.record?.provider,
    mono: true,
  },
  {
    label: t('usage.fields.endpoint'),
    value: props.record?.route,
    mono: true,
  },
  {
    label: t('usage.error.requestModel'),
    value: props.record
      ? usageModelDisplay(props.record).primary
      : null,
    mono: true,
  },
  {
    label: t('usage.error.upstreamModel'),
    value: props.record?.upstreamModel,
    mono: true,
  },
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
])
const identifierItems = computed(() => [
  {
    label: t('usage.error.clientStatusCode'),
    value: props.record?.clientStatusCode,
    mono: true,
  },
  {
    label: t('usage.error.upstreamStatusCode'),
    value: props.record?.upstreamStatusCode,
    mono: true,
  },
  {
    label: t('usage.error.requestId'),
    value: props.record?.requestId,
    mono: true,
    fullWidth: true,
  },
  ...(props.record?.upstreamRequestId
    ? [
        {
          label: t('usage.error.upstreamRequestId'),
          value: props.record.upstreamRequestId,
          mono: true,
          fullWidth: true,
        },
      ]
    : []),
  {
    label: t('usage.error.responseId'),
    value: props.record?.responseId,
    mono: true,
    fullWidth: true,
  },
  {
    label: t('usage.error.clientKeyId'),
    value: props.record?.clientApiKeyId,
    mono: true,
    fullWidth: true,
  },
])
const attempts = computed(() => (props.record?.attempts ?? []).map(attempt => ({
  id: attempt.id,
  index: attempt.attemptIndex,
  outcome: usageDisplayOutcome(attempt.outcome, attempt.providerErrorCode, attempt.failureClass),
  statusCode: attempt.statusCode,
  items: [
    {
      label: t('usage.error.errorCode'),
      value: attempt.providerErrorCode,
      mono: true,
    },
    {
      label: t('usage.error.failureClassification'),
      value: attempt.failureClass,
      mono: true,
    },
    {
      label: t('accounts.account'),
      value: usageAccountText(attempt),
      mono: true,
    },
    {
      label: t('usage.error.platform'),
      value: attempt.provider,
      mono: true,
    },
    {
      label: t('usage.error.upstreamTransport'),
      value: usageTransportType(attempt.transport),
      mono: true,
    },
    {
      label: t('usage.error.duration'),
      value: formatDuration(attempt.latencyMs),
      mono: true,
    },
    {
      label: t('usage.error.triggerMode'),
      value: attempt.trigger,
      mono: true,
    },
    {
      label: t('usage.error.sendStatus'),
      value: attempt.sendState,
      mono: true,
    },
  ],
})))
</script>

<template>
  <ZDialog v-model="open" :title="t('usage.error.errorDetails')" :description="t('usage.error.description')" type="danger" width="64rem">
    <UsageDetailState v-if="loading || error" variant="error" :loading="!!loading" :error="error" @retry="emit('retry')" />
    <div v-else-if="record" class="grid min-w-0 gap-3">
      <section class="min-w-0 rounded-z-card bg-z-error-container px-4 py-3.5 text-z-error-on-container">
        <div class="flex flex-wrap items-center gap-3">
          <h3 class="m-0 text-z-sm leading-none font-heavy">
            {{ errorTitle }}
          </h3>
          <span v-if="record.recovered" class="rounded-z-sm bg-z-success-container px-2 py-1 text-z-xs font-emphasis text-z-success-on-container">
            {{ t('usage.error.recovered') }}
          </span>
        </div>
        <p v-if="errorSummary" class="mt-2 mb-0 text-z-sm leading-relaxed">
          {{ errorSummary }}
        </p>
        <UsageDetailFieldGrid v-if="errorItems.length" :items="errorItems" />
        <template v-if="upstreamMessage">
          <p class="mt-3 mb-1 text-z-xs text-z-text-secondary">
            Message
          </p>
          <p class="m-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed" translate="no">
            {{ upstreamMessage }}
          </p>
        </template>
        <template v-if="record.upstreamError?.closeReason">
          <p class="mt-3 mb-1 text-z-xs text-z-text-secondary">
            {{ t('usage.error.closeReason') }}
          </p>
          <p class="m-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed" translate="no">
            {{ record.upstreamError.closeReason }}
          </p>
        </template>
        <template v-if="!upstreamMessage || upstreamMessage !== errorMessage">
          <p class="mt-3 mb-1 text-z-xs text-z-text-secondary">
            {{ t('usage.error.gatewayMessage') }}
          </p>
          <p class="m-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed" translate="no">
            {{ errorMessage }}
          </p>
        </template>
      </section>

      <section class="grid min-w-0 gap-3 lg:grid-cols-2">
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.error.requestContext') }}
          </h3>
          <UsageDetailFieldGrid :items="contextItems" />
        </section>
        <section :class="panelClass">
          <h3 :class="panelTitleClass">
            {{ t('usage.error.errorLocation') }}
          </h3>
          <UsageDetailFieldGrid :items="identifierItems" />
        </section>
      </section>

      <section v-if="attempts.length || !record.attemptsComplete" :class="panelClass">
        <h3 :class="panelTitleClass">
          {{ t('usage.error.attempts') }}
        </h3>
        <p v-if="!record.attemptsComplete" class="mt-3 mb-0 text-z-sm text-z-text-tertiary">
          {{ t('usage.error.attemptRecordMayBeIncomplete') }}
        </p>
        <ol v-if="attempts.length" class="mt-3 mb-0 grid list-none gap-4 p-0 lg:grid-cols-2">
          <li v-for="attempt in attempts" :key="attempt.id" class="min-w-0">
            <div class="flex items-center gap-3 text-z-sm font-bold">
              <span>{{ t('usage.error.attempt', { index: attempt.index }) }}</span>
              <span :class="attemptOutcomeClass(attempt.outcome)">{{ attemptOutcomeText(attempt.outcome) }}</span>
              <UsageStatusCodeBadge :status-code="attempt.statusCode" />
            </div>
            <UsageDetailFieldGrid :items="attempt.items" />
          </li>
        </ol>
      </section>

      <details v-if="responseText || metadataText" :class="panelClass">
        <summary class="cursor-pointer text-z-sm font-heavy text-z-text-secondary">
          {{ t('usage.error.diagnosticData') }}
        </summary>
        <div class="mt-3 grid min-w-0 gap-3">
          <UsageDetailCodePanel v-if="responseText" :title="t('usage.error.responseContent')" max-height="180px" :content="responseText" />
          <UsageDetailCodePanel v-if="metadataText" :title="t('usage.error.metadata')" max-height="240px" :content="metadataText" />
        </div>
      </details>
    </div>

    <template #footer>
      <DiagnosticsDownloadButton :request-id="record?.requestId" :active="open && !loading" />
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
