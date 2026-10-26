<script setup lang="ts">
import type { UsageRecordDetail } from '@/api'

import { ZButton, ZDialog } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageAccountText, usageModelDisplay, usageTransportType } from '@/components/usage/shared/presenter'
import { formatDuration } from '@/utils/format'
import { attemptOutcomeClass, attemptOutcomeText, visibleResponseText } from '../utils/detail'
import DiagnosticsDownloadButton from './DiagnosticsDownloadButton.vue'
import UsageDetailCodePanel from './UsageDetailCodePanel.vue'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{ record: UsageRecordDetail | null }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })

const panelClass = 'min-w-0 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5'
const panelTitleClass = 'm-0 text-cp-sm leading-none font-heavy text-cp-text-secondary'

const errorMessage = computed(() => {
  const message = props.record?.message.trim()
  return message && message !== props.record?.logicalOutcome
    ? message
    : t('usage.error.noDetails')
})
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
  {
    label: t('usage.error.upstreamRequestId'),
    value: props.record?.upstreamRequestId,
    mono: true,
    fullWidth: true,
  },
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
  outcome: attempt.outcome,
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
    <div v-if="record" class="grid min-w-0 gap-3">
      <section class="min-w-0 rounded-cp-card bg-cp-error-container px-4 py-3.5 text-cp-error-on-container">
        <div class="flex items-center gap-3">
          <h3 class="m-0 text-cp-sm leading-none font-heavy">
            {{ record.logicalOutcome === 'incomplete' ? t('usage.error.responseIsNotComplete') : t('usage.error.requestFailed') }}
          </h3>
          <UsageStatusCodeBadge v-if="record.statusCode !== null" :status-code="record.statusCode" />
        </div>
        <p class="mt-3 mb-0 wrap-anywhere whitespace-pre-wrap font-mono text-cp-sm leading-relaxed">
          {{ errorMessage }}
        </p>
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
        <p v-if="!record.attemptsComplete" class="mt-3 mb-0 text-cp-sm text-cp-text-tertiary">
          {{ t('usage.error.attemptRecordMayBeIncomplete') }}
        </p>
        <ol v-if="attempts.length" class="mt-3 mb-0 grid list-none gap-4 p-0 lg:grid-cols-2">
          <li v-for="attempt in attempts" :key="attempt.id" class="min-w-0">
            <div class="flex items-center gap-3 text-cp-sm font-bold">
              <span>{{ t('usage.error.attempt', { index: attempt.index }) }}</span>
              <span :class="attemptOutcomeClass(attempt.outcome)">{{ attemptOutcomeText(attempt.outcome) }}</span>
              <UsageStatusCodeBadge :status-code="attempt.statusCode" />
            </div>
            <UsageDetailFieldGrid :items="attempt.items" />
          </li>
        </ol>
      </section>

      <details v-if="responseText || metadataText" :class="panelClass">
        <summary class="cursor-pointer text-cp-sm font-heavy text-cp-text-secondary">
          {{ t('usage.error.diagnosticData') }}
        </summary>
        <div class="mt-3 grid min-w-0 gap-3">
          <UsageDetailCodePanel v-if="responseText" :title="t('usage.error.responseContent')" max-height="180px" :content="responseText" />
          <UsageDetailCodePanel v-if="metadataText" :title="t('usage.error.metadata')" max-height="240px" :content="metadataText" />
        </div>
      </details>
    </div>

    <template #footer>
      <DiagnosticsDownloadButton v-if="record" :request-id="record.requestId" :active="open" />
      <ZButton type="primary" @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
