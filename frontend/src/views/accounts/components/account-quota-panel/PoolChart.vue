<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { EChartsType } from 'echarts/core'
import type { PoolNode } from './pool'
import { ZPopover } from '@codex-proxy/ui'
import { usePreferredReducedMotion, useTimeoutFn } from '@vueuse/core'
import { CustomChart } from 'echarts/charts'
import { DataZoomInsideComponent } from 'echarts/components'
import { use } from 'echarts/core'
import { computed, nextTick, onBeforeUnmount, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountQuotaWindowDetails from '@/components/account/AccountQuotaWindowDetails.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useThemeColor } from '@/composables/useThemeColor'
import { isRecord } from '@/utils/data'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'

const props = defineProps<{ nodes: PoolNode[] }>()
const MIN_VISIBLE_NODES = 48
use([CustomChart, DataZoomInsideComponent])
const { t } = useI18n({ useScope: 'global' })
const color = useThemeColor()
const reducedMotion = usePreferredReducedMotion()
const anchor = useTemplateRef<HTMLElement>('anchor')
const popover = useTemplateRef<InstanceType<typeof ZPopover>>('popover')
const activeIndex = shallowRef<number | null>(null)
const anchorX = shallowRef(0)
const open = shallowRef(false)
const visibleCount = shallowRef(props.nodes.length)
const activeNode = computed(() => activeIndex.value === null
  ? undefined
  : props.nodes[activeIndex.value])
const activeWindows = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(activeNode.value?.windows ?? []),
).flatMap(entry => entry.windows))
const canZoom = computed(() => props.nodes.length > MIN_VISIBLE_NODES)
let chart: EChartsType | undefined
let highlightedIndex: number | null = null
let range = { start: 0, end: 100 }
const { start: scheduleOpen, stop: cancelOpen } = useTimeoutFn(() => {
  open.value = Boolean(activeNode.value?.windows.length)
}, 180, { immediate: false })

const option = computed<EChartsOption>(() => {
  const colors = {
    success: color('--cp-color-success', ''),
    warning: color('--cp-color-warning', ''),
    danger: color('--cp-color-error', ''),
    unknown: color('--cp-color-border', ''),
  }
  const nodes = props.nodes
  return {
    animation: reducedMotion.value !== 'reduce',
    animationDuration: 0,
    animationDurationUpdate: 0,
    stateAnimation: {
      duration: 150,
      // zrender 支持 CSS 曲线但类型未列出，沿用 Popover 的 ease-out
      easing: 'cubic-bezier(0, 0, 0.2, 1)' as NonNullable<EChartsOption['stateAnimation']>['easing'],
    },
    grid: { left: 0, right: 0, top: 0, bottom: 0 },
    xAxis: {
      type: 'value',
      show: false,
      min: -0.5,
      max: Math.max(nodes.length, MIN_VISIBLE_NODES) - 0.5,
    },
    yAxis: {
      type: 'value',
      show: false,
      min: 0,
      max: 1,
    },
    dataZoom: [
      {
        id: 'pool-range',
        type: 'inside',
        xAxisIndex: 0,
        ...range,
        disabled: !canZoom.value,
        minValueSpan: MIN_VISIBLE_NODES,
        filterMode: 'weakFilter',
        zoomOnMouseWheel: true,
        moveOnMouseMove: true,
        moveOnMouseWheel: false,
        throttle: 40,
      },
    ],
    series: [
      {
        type: 'custom',
        animationThreshold: nodes.length,
        clip: true,
        dimensions: ['account', 'value'],
        encode: { x: 0, y: 1 },
        data: nodes.map((node, index) => ({
          id: node.id,
          value: [index, 1],
        })),
        renderItem: (_params, api) => {
          const index = Number(api.value(0))
          const x = api.coord([index, 0])[0]!
          const pitch = api.coord([index + 1, 0])[0]! - x
          const width = Math.min(8, pitch * 0.72)
          const fill = colors[nodes[index]!.tone]
          return {
            type: 'rect',
            shape: {
              x: x - width / 2,
              y: 7,
              width,
              height: 22,
              r: Math.min(2, width / 2),
            },
            style: { fill, stroke: fill, lineWidth: 0 },
            emphasis: {
              style: { fill, stroke: fill, lineWidth: 2 },
            },
          }
        },
      },
    ],
  }
})

function onDataZoom(event: unknown) {
  if (!isRecord(event))
    return
  const value = Array.isArray(event.batch)
    ? event.batch[0]
    : event
  if (!isRecord(value) || typeof value.start !== 'number' || typeof value.end !== 'number')
    return
  range = { start: value.start, end: value.end }
  visibleCount.value = Math.ceil(props.nodes.length * (range.end - range.start) / 100)
  cancelOpen()
  clearHighlight()
  open.value = false
  activeIndex.value = null
}

function onReady(instance: EChartsType) {
  chart?.off('datazoom', onDataZoom)
  chart = instance
  chart.on('datazoom', onDataZoom)
}

function zoom(factor: number, index?: number) {
  if (!canZoom.value)
    return
  const span = Math.min(100, Math.max(MIN_VISIBLE_NODES / props.nodes.length * 100, (range.end - range.start) * factor))
  const center = index === undefined
    ? (range.start + range.end) / 2
    : (index + 0.5) / props.nodes.length * 100
  const start = Math.max(0, Math.min(100 - span, center - span / 2))
  chart?.dispatchAction({
    type: 'dataZoom',
    dataZoomId: 'pool-range',
    start,
    end: start + span,
  })
}

function resetZoom() {
  chart?.dispatchAction({
    type: 'dataZoom',
    dataZoomId: 'pool-range',
    start: 0,
    end: 100,
  })
}

function clearHighlight() {
  if (highlightedIndex !== null)
    chart?.dispatchAction({ type: 'downplay', seriesIndex: 0, dataIndex: highlightedIndex })
  highlightedIndex = null
}

function onPointerLeave() {
  cancelOpen()
  clearHighlight()
}

function selectNode(index: number | null) {
  if (index === activeIndex.value && index === highlightedIndex)
    return
  cancelOpen()
  clearHighlight()
  activeIndex.value = index
  if (index === null) {
    open.value = false
    return
  }
  chart?.dispatchAction({ type: 'highlight', seriesIndex: 0, dataIndex: index })
  highlightedIndex = index
  anchorX.value = Number(chart?.convertToPixel({ xAxisIndex: 0 }, index))
  if (activeNode.value?.windows.length) {
    if (!open.value)
      scheduleOpen()
    void nextTick(() => popover.value?.updatePosition())
  }
  else {
    open.value = false
  }
}

function onPointerMove(event: PointerEvent) {
  if (!chart || event.buttons)
    return
  const x = event.clientX - chart.getDom().getBoundingClientRect().left
  const index = Math.round(Number(chart.convertFromPixel({ xAxisIndex: 0 }, x)))
  selectNode(index >= 0 && index < props.nodes.length
    ? index
    : null)
}

function onChartClick() {
  if (visibleCount.value > 80 && activeIndex.value !== null)
    zoom(80 / visibleCount.value, activeIndex.value)
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === '+' || event.key === '=') {
    zoom(0.5)
  }
  else if (event.key === '-') {
    zoom(2)
  }
  else if (event.key === 'Home') {
    resetZoom()
  }
  else if (event.key === 'Escape') {
    cancelOpen()
    open.value = false
  }
  else if (event.key === 'Enter') {
    onChartClick()
  }
  else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    const index = Math.max(0, Math.min(props.nodes.length - 1, (activeIndex.value ?? Math.floor(props.nodes.length * range.start / 100)) + (event.key === 'ArrowLeft'
      ? -1
      : 1)))
    if (index / props.nodes.length * 100 < range.start || (index + 1) / props.nodes.length * 100 > range.end)
      zoom(1, index)
    selectNode(index)
  }
  else {
    return
  }
  event.preventDefault()
  event.stopPropagation()
}

watch(() => props.nodes, () => {
  range = { start: 0, end: 100 }
  visibleCount.value = props.nodes.length
  selectNode(null)
})
onBeforeUnmount(() => chart?.off('datazoom', onDataZoom))
</script>

<template>
  <div class="min-w-0">
    <ZPopover ref="popover" v-model="open" class="w-full" placement="top" trigger="hover-click" :reference-element="anchor" :disabled="!activeNode?.windows.length" :hover-delay="180" animate-position>
      <template #reference>
        <div
          class="relative w-full rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          role="slider" tabindex="0" :aria-valuemin="1" :aria-valuemax="nodes.length" :aria-valuenow="(activeIndex ?? 0) + 1" :aria-valuetext="t('accounts.upstream.account', { number: (activeIndex ?? 0) + 1 })" :aria-label="t('accounts.upstream.poolChart')" :aria-description="t('accounts.upstream.poolKeyboard')"
          @pointermove="onPointerMove" @pointerleave="onPointerLeave" @click="onChartClick" @dblclick="resetZoom" @keydown="onKeydown"
        >
          <BaseChart :option="option" :height="36" @ready="onReady" />
          <span ref="anchor" class="pointer-events-none absolute top-1.5 h-6 w-px" :style="{ left: `${anchorX}px` }" aria-hidden="true" />
        </div>
      </template>
      <AccountQuotaWindowDetails v-if="activeNode?.windows.length" :label="t('accounts.upstream.nodeQuota')" :windows="activeWindows" :show-consumption="false" />
    </ZPopover>
  </div>
</template>
