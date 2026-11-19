<script setup lang="ts">
import type { UsageRecordStatus } from '@/api/modules/usage'

import { ZSegmented } from '@codex-proxy/ui'

import { CloudRain, List, Sun } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ showLabels?: boolean }>()
const { t } = useI18n({ useScope: 'global' })

const model = defineModel<UsageRecordStatus>({ required: true })
const options = computed(() => ([
  {
    label: t('common.status.success'),
    value: 'success',
    icon: props.showLabels ? undefined : Sun,
  },
  {
    label: t('usage.filters.error'),
    value: 'error',
    icon: props.showLabels ? undefined : CloudRain,
  },
  {
    label: t('common.values.all'),
    value: 'all',
    icon: props.showLabels ? undefined : List,
  },
]))
</script>

<template>
  <ZSegmented v-model="model" :aria-label="t('usage.filters.requestDetailType')" :options="options" :display="showLabels ? 'label' : 'icon'" :class="{ 'w-30 shrink-0': !showLabels }" />
</template>
