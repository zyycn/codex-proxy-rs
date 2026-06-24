<script setup lang="ts">
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
const observer = shallowRef<ResizeObserver>()
const animationFrame = shallowRef<number>()

const style = computed(() => ({
  height: `${props.height}px`,
}))

function elementHasSize(element: HTMLElement) {
  return element.clientWidth > 0 && element.clientHeight > 0
}

function cancelPendingInit() {
  if (animationFrame.value !== undefined) {
    cancelAnimationFrame(animationFrame.value)
    animationFrame.value = undefined
  }
}

function ensureChart(element: HTMLElement) {
  if (chart.value || !elementHasSize(element)) return
  chart.value = init(element, undefined, { renderer: 'canvas' })
  chart.value.setOption(chartOption.value)
}

function scheduleInit(element: HTMLElement) {
  cancelPendingInit()
  animationFrame.value = requestAnimationFrame(() => {
    animationFrame.value = undefined
    ensureChart(element)
  })
}

function resize() {
  chart.value?.resize()
}

function dispose() {
  cancelPendingInit()
  observer.value?.disconnect()
  chart.value?.dispose()
  observer.value = undefined
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
    observer.value = new ResizeObserver(() => {
      if (chart.value) {
        resize()
        return
      }
      ensureChart(element)
    })
    observer.value.observe(element)
    scheduleInit(element)
  },
  { immediate: true },
)

onBeforeUnmount(dispose)
</script>

<template>
  <div ref="chart" class="w-full min-w-0" :style="style" />
</template>
