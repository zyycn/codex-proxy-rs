<script setup lang="ts">
import type { UsageRecordStatus } from '@/api/modules/usage'

import { ZSegmented } from '@codex-proxy/ui'

import { CloudRain, List, Sun } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<UsageRecordStatus>({ required: true })
const options = computed(() => ([
  {
    label: t('common.status.success'),
    value: 'success',
    icon: Sun,
  },
  {
    label: t('usage.filters.error'),
    value: 'error',
    icon: CloudRain,
  },
  {
    label: t('common.values.all'),
    value: 'all',
    icon: List,
  },
]))
</script>

<template>
  <ZSegmented v-model="model" :aria-label="t('usage.filters.requestDetailType')" :options="options" display="icon" class="w-30 shrink-0" />
</template>
