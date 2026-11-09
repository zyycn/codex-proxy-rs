<script setup lang="ts">
import type { UsageRecordDetail } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageErrorOrigin, usageErrorTitle } from '@/components/usage/shared/errors'
import { usageTransportType } from '@/components/usage/shared/presenter'
import { formatDuration } from '@/utils/format'
import UsageDetailFieldGrid from './UsageDetailFieldGrid.vue'
import UsageStatusCodeBadge from './UsageStatusCodeBadge.vue'

const props = defineProps<{ record: UsageRecordDetail }>()
const { t } = useI18n({ useScope: 'global' })
const origin = computed(() => usageErrorOrigin(props.record))
const title = computed(() => usageErrorTitle(props.record))
const gatewayMessage = computed(() => {
  const message = props.record.message.trim()
  return message && message !== props.record.logicalOutcome
    ? message
    : t('usage.error.noDetails')
})
const gatewayItems = computed(() => [
  {
    label: t('usage.error.failureClassification'),
    value: props.record.errorKind,
  },
  {
    label: t('usage.error.continuationReason'),
    value: props.record.continuationUnavailableReason,
  },
  {
    label: t('usage.error.connectionExitReason'),
    value: props.record.upstreamConnectionExitReason,
  },
].filter(item => item.value).map(item => ({ ...item, mono: true, wrap: true })))
const upstreamItems = computed(() => [
  { label: 'Code', value: props.record.upstreamError?.code ?? props.record.errorCode },
  { label: 'Type', value: props.record.upstreamError?.errorType },
  { label: 'Param', value: props.record.upstreamError?.param },
  {
    label: t('usage.error.websocketCloseCode'),
    value: props.record.websocketCloseCode,
  },
  {
    label: t('usage.error.lastEventType'),
    value: props.record.upstreamError?.lastEventType,
  },
  {
    label: t('usage.error.retryAfter'),
    value: props.record.retryAfterMs == null
      ? null
      : formatDuration(props.record.retryAfterMs),
  },
].filter(item => item.value != null && item.value !== '').map(item => ({ ...item, mono: true, wrap: true })))
const successfulHttpStatus = computed(() => [props.record.clientStatusCode, props.record.upstreamStatusCode]
  .some(status => status != null && status >= 200 && status < 300))
</script>

<template>
  <section class="grid min-w-0 gap-3">
    <div class="rounded-z-card bg-z-fill-quaternary px-4 py-3.5">
      <div class="flex flex-wrap items-center gap-2">
        <span class="rounded-z-sm px-2 py-1 text-z-sm font-bold" :class="origin.className">{{ origin.label }}</span>
        <span class="text-z-sm font-bold text-z-error-text">
          {{ title }}
        </span>
        <span v-if="record.recovered" class="rounded-z-sm bg-z-success-container px-2 py-1 text-z-xs font-emphasis text-z-success-on-container">
          {{ t('usage.error.recovered') }}
        </span>
      </div>
      <p v-if="origin.hint" class="mt-2 mb-0 text-z-sm leading-relaxed text-z-text-secondary">
        {{ origin.hint }}
      </p>
      <p v-if="record.recovered" class="mt-2 mb-0 text-z-sm text-z-success-text">
        {{ t('usage.error.recoveryExplanation') }}
      </p>
    </div>

    <div class="grid min-w-0 gap-3 md:grid-cols-2">
      <section class="min-w-0 rounded-z-card bg-z-fill-quaternary px-4 py-3.5">
        <h3 class="m-0 text-z-sm font-heavy text-z-text">
          {{ t('usage.error.gatewayMessage') }}
        </h3>
        <p class="mt-3 mb-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed text-z-text" translate="no">
          {{ gatewayMessage }}
        </p>
        <UsageDetailFieldGrid v-if="gatewayItems.length" :items="gatewayItems" />
      </section>

      <section class="min-w-0 rounded-z-card bg-z-fill-quaternary px-4 py-3.5">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <h3 class="m-0 text-z-sm font-heavy text-z-text">
            {{ t('usage.error.upstreamEvidence') }}
          </h3>
          <span v-if="record.upstreamStatusCode != null" class="inline-flex items-center gap-2 text-z-xs text-z-text-secondary" :title="t('usage.error.upstreamStatusCode')">
            HTTP <UsageStatusCodeBadge :status-code="record.upstreamStatusCode" />
          </span>
        </div>
        <p v-if="record.upstreamError?.message" class="mt-3 mb-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed text-z-text" translate="no">
          {{ record.upstreamError.message }}
        </p>
        <div v-else-if="record.upstreamError?.closeReason" class="mt-3">
          <p class="m-0 text-z-xs text-z-text-secondary">
            {{ t('usage.error.closeReason') }}
          </p>
          <p class="mt-1 mb-0 wrap-anywhere whitespace-pre-wrap font-mono text-z-sm leading-relaxed text-z-text" translate="no">
            {{ record.upstreamError.closeReason }}
          </p>
        </div>
        <p v-else class="mt-3 mb-0 text-z-sm text-z-text-tertiary">
          {{ t('usage.error.noUpstreamMessage') }}
        </p>
        <UsageDetailFieldGrid v-if="upstreamItems.length" :items="upstreamItems" />
      </section>
    </div>

    <div class="flex flex-wrap items-center gap-x-3 gap-y-2 px-1 text-z-sm text-z-text-secondary">
      <span class="font-bold">{{ t('usage.error.clientResult') }}</span>
      <span>{{ usageTransportType(record.clientTransport) }}</span>
      <span v-if="record.clientStatusCode != null" class="inline-flex items-center gap-2" :title="t('usage.error.clientStatusCode')">
        HTTP <UsageStatusCodeBadge :status-code="record.clientStatusCode" />
      </span>
      <span v-else>{{ t('usage.error.noHttpStatus') }}</span>
      <span v-if="successfulHttpStatus" class="basis-full text-z-xs">{{ t('usage.error.httpStatusHint') }}</span>
    </div>
  </section>
</template>
