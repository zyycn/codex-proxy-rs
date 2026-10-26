<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'

import { Globe } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { countryFlagUrl, countryName, proxyExitLocation } from '../utils/location'

const props = defineProps<{ proxy: OutboundProxyRecord }>()

const { t } = useI18n({ useScope: 'global' })

const location = computed(() => proxyExitLocation(props.proxy))
const customLocation = computed(() => props.proxy.autoLocation ? null : props.proxy.location)
const customLabel = computed(() => customLocation.value
  ? `${countryName(customLocation.value.country)} · ${customLocation.value.city}`
  : '')
const flag = computed(() => location.value
  ? countryFlagUrl(location.value.country)
  : undefined)
const label = computed(() => {
  if (props.proxy.lastTest?.success === false)
    return t('proxies.region.connectionFailed')
  if (location.value)
    return `${countryName(location.value.country)} · ${location.value.city}`
  const detection = props.proxy.lastTest?.location
  if (detection?.status === 'conflict')
    return t('proxies.region.regionalConflict')
  if (detection?.status === 'failed')
    return t('proxies.region.parsingFailed')
  return t('proxies.region.exitNotRecognized')
})
const description = computed(() => {
  if (props.proxy.lastTest?.success === false)
    return props.proxy.lastTest.message
  const detection = props.proxy.lastTest?.location
  if (detection?.status === 'failed')
    return detection.message
  if (detection?.status === 'conflict')
    return t('proxies.region.ipv4AndIpv6EgressTimeZonesAre')
  return location.value
    ? t('proxies.region.exitLocation', { label: label.value, timezone: location.value.timezone })
    : t('proxies.region.detectHelp')
})
</script>

<template>
  <span class="grid min-w-0 grid-cols-[1.125rem_minmax(0,1fr)] items-center gap-x-2 gap-y-1" :class="location ? 'text-cp-text' : 'text-cp-text-tertiary'" :title="description">
    <img v-if="flag" :src="flag" alt="" aria-hidden="true" class="size-4.5 shrink-0" loading="lazy" decoding="async">
    <Globe v-else class="size-4 shrink-0 text-cp-text-tertiary" aria-hidden="true" />
    <span class="truncate">{{ label }}</span>
    <span v-if="customLocation" class="col-start-2 truncate text-cp-xs text-cp-text-secondary" :title="t('proxies.region.customLocation', { customLabel, timezone: customLocation.timezone })">
      {{ t('proxies.region.custom', { customLabel }) }}
    </span>
  </span>
</template>
