<script setup lang="ts">
import type { DashboardHealthTimeline, DashboardHealthTimelinePoint } from '@/api'

import { ZButton, ZCard, ZPopover } from '@codex-proxy/ui'
import { usePreferredReducedMotion, useResizeObserver } from '@vueuse/core'

import { gsap } from 'gsap'
import { computed, onBeforeUnmount, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import HealthTimelinePointPopover from '@/components/usage/HealthTimelinePointPopover.vue'
import { formatHealthCount, healthEligibleRequestCount, healthLegend, healthObservedRequestCount, healthStatusMeta } from '@/components/usage/shared/health'

const props = defineProps<{
  timeline: DashboardHealthTimeline
}>()

const { t } = useI18n({ useScope: 'global' })

const healthPopoverArrowSurfaceClasses = {
  top: 'bg-cp-popover-header-bg',
  bottom: 'bg-cp-bg-elevated',
}

const timelineGrid = useTemplateRef<HTMLElement>('timelineGrid')
const popoverAnchor = useTemplateRef<HTMLElement>('popoverAnchor')
const pointPopover = useTemplateRef<InstanceType<typeof ZPopover>>('pointPopover')
const preferredMotion = usePreferredReducedMotion()
const points = computed(() => props.timeline.points)

const activePoint = shallowRef<DashboardHealthTimelinePoint>()
const popoverOpen = shallowRef(false)
let wavedCellIndexes = new Set<number>()
let anchorXTo: ReturnType<typeof gsap.quickTo> | undefined
let anchorYTo: ReturnType<typeof gsap.quickTo> | undefined

function isInteractivePoint(point: DashboardHealthTimelinePoint) {
  return point.status !== 'future' && healthObservedRequestCount(point) > 0
}

function isActivePoint(point: DashboardHealthTimelinePoint) {
  return popoverOpen.value && activePoint.value?.bucketStart === point.bucketStart
}

function activatePoint(point: DashboardHealthTimelinePoint, pointIndex: number, event: Event) {
  if (!(event.currentTarget instanceof HTMLElement))
    return
  if (!isInteractivePoint(point)) {
    closePointPopover()
    return
  }

  const anchor = event.currentTarget
  const opening = !popoverOpen.value
  activePoint.value = point
  animatePointWave(pointIndex)

  positionPointAnchor(anchor, opening)
  popoverOpen.value = true
}

function positionPointAnchor(button: HTMLElement, immediate: boolean) {
  const anchor = popoverAnchor.value
  const grid = timelineGrid.value
  if (!anchor || !grid)
    return

  const buttonRect = button.getBoundingClientRect()
  const gridRect = grid.getBoundingClientRect()
  const x = buttonRect.left + buttonRect.width / 2 - gridRect.left - 0.5
  const y = buttonRect.top - gridRect.top

  if (immediate || preferredMotion.value === 'reduce') {
    stopPointAnchor()
    gsap.set(anchor, { x, y })
  }
  if (!anchorXTo || !anchorYTo) {
    anchorXTo = gsap.quickTo(anchor, 'x', { duration: 0.16, ease: 'power3.out' })
    anchorYTo = gsap.quickTo(anchor, 'y', {
      duration: 0.16,
      ease: 'power3.out',
      // 原生锚点直接跟随变换，事件定位路径也在同一帧更新面板与箭头
      onUpdate: () => pointPopover.value?.updatePosition(),
    })
  }
  if (!immediate && preferredMotion.value !== 'reduce') {
    anchorXTo(x)
    anchorYTo(y)
  }
}

function stopPointAnchor() {
  if (popoverAnchor.value)
    gsap.killTweensOf(popoverAnchor.value)
  anchorXTo = undefined
  anchorYTo = undefined
}

function closePointPopover() {
  pointPopover.value?.close()
}

function timelineButtons() {
  return Array.from(timelineGrid.value?.querySelectorAll<HTMLButtonElement>('[data-health-timeline-point]') ?? [])
}

function timelineCell(button?: HTMLButtonElement) {
  return button?.querySelector<HTMLElement>('[data-health-timeline-cell]')
}

function animatePointWave(centerIndex: number) {
  const buttons = timelineButtons()
  const centerButton = buttons[centerIndex]
  if (!centerButton)
    return

  const centerRow = centerButton.offsetTop
  const nextCellIndexes = new Set<number>()
  const cellsByDistance: HTMLElement[][] = [[], [], []]

  for (let distance = 0; distance <= 2; distance += 1) {
    const candidateIndexes = distance === 0
      ? [centerIndex]
      : [centerIndex - distance, centerIndex + distance]

    for (const index of candidateIndexes) {
      const button = buttons[index]
      const point = points.value[index]
      const cell = timelineCell(button)
      if (!button || !point || !cell || button.offsetTop !== centerRow || !isInteractivePoint(point)) {
        continue
      }

      nextCellIndexes.add(index)
      cellsByDistance[distance]?.push(cell)
    }
  }

  const cellsToRelease = [...wavedCellIndexes]
    .filter(index => !nextCellIndexes.has(index))
    .map(index => timelineCell(buttons[index]))
    .filter((cell): cell is HTMLElement => Boolean(cell))

  if (preferredMotion.value === 'reduce') {
    const cells = cellsByDistance.flat()
    gsap.set([
      ...cells,
      ...cellsToRelease,
    ], { clearProps: 'transform,willChange' })
    wavedCellIndexes = nextCellIndexes
    return
  }

  animateWaveCells(cellsByDistance[0] ?? [], -6, 1.16, 0.26, 'back.out(1.45)')
  animateWaveCells(cellsByDistance[1] ?? [], -3.25, 1.08, 0.34, 'power3.out')
  animateWaveCells(cellsByDistance[2] ?? [], -1.25, 1.03, 0.42, 'power3.out')
  settleWaveCells(cellsToRelease)
  wavedCellIndexes = nextCellIndexes
}

function animateWaveCells(cells: HTMLElement[], y: number, scaleY: number, duration: number, ease: string) {
  if (cells.length === 0)
    return

  gsap.set(cells, { willChange: 'transform' })
  gsap.to(cells, {
    y,
    scaleY,
    duration,
    ease,
    overwrite: 'auto',
  })
}

function settleWaveCells(cells: HTMLElement[]) {
  if (cells.length === 0)
    return

  gsap.to(cells, {
    y: 0,
    scaleY: 1,
    duration: 0.5,
    ease: 'power3.out',
    overwrite: 'auto',
    clearProps: 'transform,willChange',
  })
}

function releasePointWave() {
  const buttons = timelineButtons()
  const cells = [...wavedCellIndexes]
    .map(index => timelineCell(buttons[index]))
    .filter((cell): cell is HTMLElement => Boolean(cell))

  if (preferredMotion.value === 'reduce') {
    gsap.set(cells, { clearProps: 'transform,willChange' })
  }
  else {
    settleWaveCells(cells)
  }
  wavedCellIndexes = new Set<number>()
}

function pointAccessibilityLabel(point: DashboardHealthTimelinePoint) {
  const eligibleRequests = healthEligibleRequestCount(point)
  return t('usage.health.validRequestsAvailability', {
    time: point.time,
    label: healthStatusMeta.value[point.status].label,
    count: formatHealthCount(eligibleRequests),
    reliabilityDisplay: point.reliabilityDisplay,
  })
}

watch(popoverOpen, (open) => {
  if (!open) {
    releasePointWave()
    stopPointAnchor()
  }
})

useResizeObserver(timelineGrid, () => {
  if (!popoverOpen.value || !activePoint.value)
    return
  const index = points.value.findIndex(point => point.bucketStart === activePoint.value?.bucketStart)
  const button = timelineButtons()[index]
  if (button)
    positionPointAnchor(button, true)
})

onBeforeUnmount(() => {
  const cells = timelineButtons()
    .map(button => timelineCell(button))
    .filter((cell): cell is HTMLElement => Boolean(cell))
  gsap.killTweensOf(cells)
  stopPointAnchor()
})
</script>

<template>
  <ZCard as="article" :title="t('dashboard.health.title')" :description="t('dashboard.health.description')" class="w-full">
    <template #actions>
      <div class="flex w-full flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div
          class="flex max-w-full flex-wrap items-center gap-x-2 gap-y-1 text-cp-xs leading-none font-emphasis text-cp-text-quaternary"
        >
          <span
            v-for="item in healthLegend"
            :key="item.status"
            class="inline-flex h-3.5 items-center gap-1 align-middle leading-none"
          >
            <span class="block size-2 shrink-0 rounded-xs" :class="healthStatusMeta[item.status].cellClass" />
            <span class="block leading-none">{{ item.label }}</span>
          </span>
        </div>
        <div class="flex shrink-0 items-center gap-3">
          <strong
            class="font-mono text-sm leading-none font-heavy tabular-nums"
            :class="healthStatusMeta[timeline.status].valueClass"
          >
            {{ timeline.reliabilityDisplay }}
          </strong>
        </div>
      </div>
    </template>

    <template #body>
      <ZPopover
        ref="pointPopover"
        v-model="popoverOpen"
        trigger="hover"
        :show-after="0"
        :disabled="!activePoint"
        class="w-full! min-w-0"
        placement="top"
        :offset="12"
        :reference-element="popoverAnchor"
        :arrow-surface-class="healthPopoverArrowSurfaceClasses"
        @closed="activePoint = undefined"
      >
        <template #reference>
          <div class="relative flex w-full flex-col">
            <!-- 同一锚点连续追向各时间格，避免切换原生锚点时的位置跳变 -->
            <span ref="popoverAnchor" aria-hidden="true" class="pointer-events-none absolute left-0 top-0 h-5 w-px opacity-0" />
            <div
              ref="timelineGrid" class="grid w-full grid-cols-[repeat(var(--timeline-columns),minmax(0,1fr))] items-end gap-x-0.5 gap-y-1 sm:grid-cols-[repeat(var(--timeline-columns-wide),minmax(0,1fr))]"
              :style="{ '--timeline-columns': Math.max(1, Math.ceil(points.length / 2)), '--timeline-columns-wide': Math.max(1, points.length) }"
            >
              <ZButton
                v-for="(point, pointIndex) in points"
                :key="point.bucketStart"
                data-health-timeline-point
                variant="text"
                :aria-disabled="!isInteractivePoint(point)"
                :tabindex="isInteractivePoint(point) ? 0 : -1"
                :aria-expanded="isActivePoint(point)"
                aria-haspopup="dialog"
                :aria-label="pointAccessibilityLabel(point)"
                class="group relative h-5! w-full min-w-0.5 bg-transparent! p-0! [&>span]:w-full"
                :class="isInteractivePoint(point) ? 'cursor-pointer' : 'cursor-default'"
                @mouseenter="activatePoint(point, pointIndex, $event)"
                @focus="activatePoint(point, pointIndex, $event)"
                @click="activatePoint(point, pointIndex, $event)"
              >
                <span
                  data-health-timeline-cell
                  class="block h-3.5 w-full origin-bottom rounded-xs transition-[filter,box-shadow] duration-250 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
                  :class="[
                    healthStatusMeta[point.status].cellClass,
                    isActivePoint(point)
                      ? 'brightness-105 shadow-[0_5px_10px_-5px_var(--cp-color-shadow)]'
                      : isInteractivePoint(point)
                        ? 'group-hover:brightness-95'
                        : undefined,
                  ]"
                />
              </ZButton>
            </div>
          </div>
        </template>
        <div data-health-timeline-popover class="w-72 overflow-hidden rounded-cp-lg">
          <HealthTimelinePointPopover v-if="activePoint" :point="activePoint" />
        </div>
      </ZPopover>
    </template>
  </ZCard>
</template>
