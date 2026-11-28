<script setup lang="ts">
import type { ProxyTargetTestRow, TargetTestProxy } from '../composables/useProxyTargetTest'
import { ZTag } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { countryName, proxyExitLocation } from '../utils/location'

const props = defineProps<{ proxy: TargetTestProxy, row?: ProxyTargetTestRow, batchRunning: boolean }>()
const { t } = useI18n({ useScope: 'global' })
const exitIp = computed(() => props.row?.result?.kind === 'http'
  ? props.row.result.exitIp
  : null)
const location = computed(() => {
  const ip = exitIp.value
  const detected = props.proxy.detectedLocation
  if (!ip || !detected || (ip !== detected.exitIpv4 && ip !== detected.exitIpv6))
    return null
  return proxyExitLocation(props.proxy)
})
const failure = computed(() => props.row?.result?.kind === 'failed'
  ? props.row.result.message
  : props.row?.error)

function httpLabel(status: number) {
  if (status >= 200 && status < 300)
    return t('common.status.success')
  if (status === 403)
    return t('proxies.targets.accessDenied')
  if (status === 429)
    return t('common.status.rateLimited')
  if (status >= 300 && status < 400)
    return t('proxies.targets.redirect')
  return ''
}
</script>

<template>
  <li class="min-w-0 rounded-z-card bg-z-fill-quaternary px-4 py-3">
    <div class="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1.5">
      <strong class="min-w-0 grow basis-44 text-z-sm font-emphasis wrap-anywhere text-z-text">{{ row?.name ?? proxy.name }}</strong>
      <ZTag v-if="row?.result?.kind === 'http'" :type="row.result.status >= 500 ? 'danger' : row.result.status >= 300 ? 'warning' : 'success'" size="small">
        HTTP {{ row.result.status }} {{ httpLabel(row.result.status) }}
      </ZTag>
      <ZTag v-else-if="row?.result?.kind === 'failed' || row?.error" type="danger" size="small">
        {{ row?.result?.kind === 'failed' ? t('proxies.region.connectionFailed') : t('common.status.failed') }}
      </ZTag>
      <span v-else-if="!row" class="text-z-xs text-z-text-secondary">{{ t('proxies.page.notTested') }}</span>
      <span v-else class="text-z-xs text-z-text-secondary">{{ row.running ? t('proxies.page.underTest') : batchRunning ? t('common.status.pending') : t('proxies.targets.skipped') }}</span>
    </div>
    <dl class="mt-3 mb-0 flex flex-wrap items-start gap-x-7 gap-y-2 text-z-sm">
      <div v-if="proxy.endpoint" class="grid min-w-0 gap-0.5">
        <dt class="text-z-xs text-z-text-secondary">
          {{ t('proxies.editor.proxyAddress') }}
        </dt>
        <dd class="m-0 font-mono font-medium wrap-anywhere text-z-text">
          {{ proxy.endpoint }}
        </dd>
      </div>
      <div class="grid min-w-0 gap-0.5">
        <dt class="text-z-xs text-z-text-secondary">
          {{ t('proxies.fields.exitIp') }}
        </dt>
        <dd class="m-0 flex flex-wrap items-baseline gap-x-1.5">
          <span class="font-mono font-medium wrap-anywhere text-z-text">{{ exitIp ?? '—' }}</span>
          <span v-if="exitIp" class="text-z-xs text-z-text-secondary">{{ exitIp.includes(':') ? 'IPv6' : 'IPv4' }}</span>
        </dd>
      </div>
      <div class="grid gap-0.5">
        <dt class="text-z-xs text-z-text-secondary">
          {{ t('proxies.fields.latency') }}
        </dt>
        <dd class="m-0 font-medium whitespace-nowrap text-z-text tabular-nums">
          {{ row?.result ? `${row.result.latencyMs} ms` : '—' }}
        </dd>
      </div>
      <div v-if="location" class="grid min-w-0 gap-0.5">
        <dt class="text-z-xs text-z-text-secondary">
          {{ t('proxies.targets.detectedRegion') }}
        </dt>
        <dd class="m-0 font-medium wrap-anywhere text-z-text">
          {{ countryName(location.country) }} · {{ location.city }}
        </dd>
      </div>
    </dl>
    <p v-if="failure" class="mt-2 mb-0 text-z-sm wrap-anywhere text-z-error-text">
      {{ failure }}
    </p>
  </li>
</template>
