<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'

import { useECharts } from '@/composables/useECharts'

const props = withDefaults(defineProps<{
  option: EChartsOption
  height?: number
}>(), {
  height: 240,
})

const chartElement = useTemplateRef<HTMLElement>('chart')
const chartOption = shallowRef<EChartsOption>(props.option)

const style = computed(() => ({
  height: `${props.height}px`,
}))

watch(
  () => props.option,
  (option) => {
    chartOption.value = option
  },
)

useECharts(chartElement, chartOption)
</script>

<template>
  <div ref="chart" class="w-full min-w-0" :style="style" />
</template>
