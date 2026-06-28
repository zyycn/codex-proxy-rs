<script setup lang="ts">
import { useRafFn, useResizeObserver } from '@vueuse/core'
import type { EChartsOption } from 'echarts'
import { init, type EChartsType } from 'echarts/core'
import { computed, onBeforeUnmount, shallowRef, useTemplateRef, watch } from 'vue'

import '@/plugins/echarts'

const props = withDefaults(
  defineProps<{
    option: EChartsOption
    height?: number
  }>(),
  {
    height: 240,
  },
)

const chartElement = useTemplateRef<HTMLElement>('chart')
const chartOption = shallowRef<EChartsOption>(props.option)
const chart = shallowRef<EChartsType>()
const pendingInitElement = shallowRef<HTMLElement>()

const style = computed(() => ({
  height: `${props.height}px`,
}))

function elementHasSize(element: HTMLElement) {
  return element.clientWidth > 0 && element.clientHeight > 0
}

function cancelPendingInit() {
  pendingInitElement.value = undefined
  pausePendingInit()
}

function ensureChart(element: HTMLElement) {
  if (chart.value || !elementHasSize(element)) return
  chart.value = init(element, undefined, { renderer: 'canvas' })
  chart.value.setOption(chartOption.value)
}

function scheduleInit(element: HTMLElement) {
  pendingInitElement.value = element
  resumePendingInit()
}

const { pause: pausePendingInit, resume: resumePendingInit } = useRafFn(
  () => {
    const element = pendingInitElement.value
    pendingInitElement.value = undefined
    if (element) {
      ensureChart(element)
    }
  },
  { immediate: false, once: true },
)

function resize() {
  chart.value?.resize()
}

function dispose() {
  cancelPendingInit()
  chart.value?.dispose()
  chart.value = undefined
}

watch(
  () => props.option,
  (option) => {
    chartOption.value = option
    if (chart.value) {
      chart.value.setOption(option, true)
      return
    }
    if (chartElement.value) {
      scheduleInit(chartElement.value)
    }
  },
)

watch(
  chartElement,
  (element) => {
    dispose()
    if (!element) return
    scheduleInit(element)
  },
  { immediate: true },
)

useResizeObserver(chartElement, () => {
  const element = chartElement.value
  if (!element) return

  if (chart.value) {
    resize()
    return
  }
  ensureChart(element)
})

onBeforeUnmount(dispose)
</script>

<template>
  <div ref="chart" class="w-full min-w-0" :style="style" />
</template>
