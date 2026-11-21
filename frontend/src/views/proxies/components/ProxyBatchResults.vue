<script setup lang="ts">
import type { ProxyBatchItem } from '../utils/batch'

import { ZScrollbar } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { proxyBatchStates, summarizeProxyBatch, summarizeProxyTests } from '../utils/batch'
import ProxyRegion from './ProxyRegion.vue'

const props = defineProps<{ items: ProxyBatchItem[], tests?: ProxyBatchItem[] }>()

const { t } = useI18n({ useScope: 'global' })

const testItems = computed(() => props.tests ?? [])

function resultMessage(item: ProxyBatchItem, test?: ProxyBatchItem) {
  if (test && test.status !== 'succeeded')
    return test.message
  const detection = test?.record?.lastTest?.location
  if (detection?.status === 'failed')
    return detection.message
  return item.status === 'succeeded'
    ? ''
    : item.message
}

const rows = computed(() => {
  const tests = new Map(testItems.value.map(item => [item.key, item]))
  return props.items.map((item) => {
    const test = item.record
      ? tests.get(item.record.id)
      : undefined
    const status = (test ?? item).status
    const state = proxyBatchStates.value[status]
    let label = state.label
    if (status === 'succeeded') {
      label = test
        ? t('proxies.batchTest.connectionIsNormal')
        : t('proxies.batchTest.addedStatus')
    }
    return {
      item,
      test,
      label,
      tone: state.tone,
      message: resultMessage(item, test),
      messageTone: status === 'succeeded'
        ? 'text-z-warning-text'
        : state.tone,
    }
  })
})

const additions = computed(() => summarizeProxyBatch(props.items))
const connections = computed(() => summarizeProxyTests(testItems.value))
const attention = computed(() => additions.value.attention + connections.value.attention)
const skipped = computed(() => additions.value.skipped + connections.value.skipped)
</script>

<template>
  <section class="min-w-0 space-y-3" :aria-label="t('proxies.batchTest.resultsTitle')">
    <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-z-xs text-z-text-secondary" role="status" aria-live="polite">
      <span>{{ t('proxies.batchTest.processed') }} <strong class="font-mono font-medium text-z-text">{{ additions.completed }} / {{ items.length }}</strong></span>
      <span>{{ t('proxies.batchTest.added', { created: additions.succeeded }) }}</span>
      <span v-if="additions.failed">{{ t('proxies.batchTest.invalidFormat', { invalid: additions.failed }) }}</span>
      <template v-if="testItems.length">
        <span>{{ t('proxies.batchTest.connected', { succeeded: connections.succeeded }) }}</span>
        <span>{{ t('proxies.batchTest.connectionFailed', { failed: connections.failed }) }}</span>
        <span v-if="connections.locationPending">{{ t('proxies.batchTest.locationPending', { locationPending: connections.locationPending }) }}</span>
      </template>
      <span v-if="attention">{{ t('proxies.batchTest.needsAttention', { attention }) }}</span>
      <span v-if="skipped">{{ t('proxies.batchTest.skipped', { skipped }) }}</span>
    </div>
    <ZScrollbar max-height="min(26rem, 50dvh)" always-visible role="region" :aria-label="t('proxies.batchTest.scrollListOfProxyResults')">
      <ol class="m-0 list-none space-y-1 p-0">
        <li v-for="row in rows" :key="row.item.key" class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 rounded-z-sm px-3 py-2.5 odd:bg-z-fill-quaternary">
          <span class="min-w-0 truncate font-mono text-z-xs text-z-text" :title="row.item.endpoint">{{ row.item.endpoint || t('proxies.batchTest.lineNumber', { key: row.item.key }) }}</span>
          <span class="flex flex-wrap justify-end gap-x-2 text-z-xs" :class="row.tone">
            <span v-if="row.test" class="text-z-text-secondary">{{ t('proxies.batchTest.addedStatus') }}</span>
            <span>{{ row.label }}</span>
          </span>
          <div class="col-span-2 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-z-xs text-z-text-secondary">
            <span class="max-w-full truncate" :title="row.item.name">{{ row.item.name }}</span>
            <ProxyRegion v-if="row.test?.record" :proxy="row.test.record" class="max-w-full" />
            <span v-if="row.test?.record?.lastTest" class="min-w-0 break-all font-mono">
              <span class="whitespace-nowrap">{{ row.test.record.lastTest.latencyMs }} ms</span><span v-if="row.test.record.lastTest.exitIpv4"> · IPv4 {{ row.test.record.lastTest.exitIpv4 }}</span><span v-if="row.test.record.lastTest.exitIpv6"> · IPv6 {{ row.test.record.lastTest.exitIpv6 }}</span>
            </span>
          </div>
          <span v-if="row.message" class="col-span-2 text-z-xs leading-relaxed wrap-anywhere" :class="row.messageTone">{{ row.message }}</span>
        </li>
      </ol>
    </ZScrollbar>
  </section>
</template>
