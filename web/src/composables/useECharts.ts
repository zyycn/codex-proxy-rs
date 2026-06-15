import { init, type EChartsType } from 'echarts/core'
import type { EChartsOption } from 'echarts'
import { onBeforeUnmount, shallowRef, watch, type ShallowRef } from 'vue'

import '@/plugins/echarts'

export function useECharts(
  target: Readonly<ShallowRef<HTMLElement | null>>,
  option: Readonly<ShallowRef<EChartsOption>>,
) {
  const chart = shallowRef<EChartsType>()
  const observer = shallowRef<ResizeObserver>()

  function resize() {
    chart.value?.resize()
  }

  function dispose() {
    observer.value?.disconnect()
    chart.value?.dispose()
    observer.value = undefined
    chart.value = undefined
  }

  watch(
    target,
    (element) => {
      dispose()
      if (!element) return
      chart.value = init(element, undefined, { renderer: 'canvas' })
      chart.value.setOption(option.value)
      observer.value = new ResizeObserver(resize)
      observer.value.observe(element)
    },
    { immediate: true },
  )

  watch(
    option,
    (nextOption) => {
      chart.value?.setOption(nextOption, true)
    },
    { deep: true },
  )

  onBeforeUnmount(dispose)

  return {
    chart,
    resize,
    dispose,
  }
}
