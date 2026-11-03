<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageTransportType } from './shared/presenter'
import UsageTransportBadge from './UsageTransportBadge.vue'

const props = defineProps<{
  upstream?: string | null
  client?: string | null
}>()
const { t } = useI18n({ useScope: 'global' })
const upstreamLabel = computed(() => usageTransportType(props.upstream))
const clientLabel = computed(() => usageTransportType(props.client))
const description = computed(() => t('usage.transportDirection', {
  upstream: upstreamLabel.value,
  client: clientLabel.value,
}))
</script>

<template>
  <span class="inline-flex items-center overflow-hidden rounded-full" tabindex="0" :aria-label="description" :title="description">
    <UsageTransportBadge :transport="upstream" joined />
    <UsageTransportBadge v-if="upstreamLabel !== clientLabel" :transport="client" joined />
  </span>
</template>
