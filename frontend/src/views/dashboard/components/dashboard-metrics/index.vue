<script setup lang="ts">
import type { DashboardCards, DashboardTrendPoint } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { metricCards } from './cards'
import MetricCard from './MetricCard.vue'

const props = defineProps<{ cards?: DashboardCards, points?: DashboardTrendPoint[] }>()
const { t } = useI18n({ useScope: 'global' })
const metrics = computed(() => metricCards(props.cards, props.points))
</script>

<template>
  <section
    class="mt-6 grid grid-cols-1 gap-4 md:grid-cols-2 2xl:grid-cols-4 2xl:gap-6"
    :aria-label="t('dashboard.metrics.title')"
  >
    <MetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
  </section>
</template>
