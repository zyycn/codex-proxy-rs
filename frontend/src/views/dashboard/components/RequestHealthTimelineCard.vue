<script setup lang="ts">
import { usePreferredReducedMotion } from '@vueuse/core'
import { gsap } from 'gsap'
import { computed, onBeforeUnmount, shallowRef, useTemplateRef, watch } from 'vue'

import BaseCard from '../../../components/base/BaseCard.vue'
import BasePopover from '../../../components/base/BasePopover.vue'
import HealthTimelinePointPopover from './HealthTimelinePointPopover.vue'
import {
  formatHealthCount,
  healthLegend,
  healthReliabilityValueClass,
  healthStatusMeta,
  type HealthTimeline,
  type HealthTimelinePoint,
} from '../constants'

const props = defineProps<{
  timeline: HealthTimeline
}>()

const timelineGrid = useTemplateRef<HTMLElement>('timelineGrid')
const preferredMotion = usePreferredReducedMotion()
const points = computed(() => props.timeline.points)

const activePoint = shallowRef<HealthTimelinePoint>()
const activeAnchor = shallowRef<HTMLElement | null>(null)
const popoverOpen = shallowRef(false)
const highlightedPointTime = shallowRef<string>()
let wavedCellIndexes = new Set<number>()

function observedRequests(point: HealthTimelinePoint) {
  return (
    point.successRequests +
    point.failedRequests +
    point.cancelledRequests +
    point.callerErrorRequests
  )
}

function isInteractivePoint(point: HealthTimelinePoint) {
  return point.status !== 'future' && observedRequests(point) > 0
}

function isActivePoint(point: HealthTimelinePoint) {
  return popoverOpen.value && highlightedPointTime.value === point.time
}

function activatePoint(point: HealthTimelinePoint, pointIndex: number, event: Event) {
  if (!(event.currentTarget instanceof HTMLElement)) return
  if (!isInteractivePoint(point)) {
    closePointPopover()
    return
  }

  activePoint.value = point
  activeAnchor.value = event.currentTarget
  popoverOpen.value = true
  highlightedPointTime.value = point.time
  animatePointWave(pointIndex)
}

function closePointPopover() {
  popoverOpen.value = false
  resetPointInteraction()
}

function resetPointInteraction() {
  highlightedPointTime.value = undefined
  activePoint.value = undefined
  activeAnchor.value = null
  releasePointWave()
}

function timelineButtons() {
  return Array.from(
    timelineGrid.value?.querySelectorAll<HTMLButtonElement>('[data-health-timeline-point]') ?? [],
  )
}

function timelineCell(button?: HTMLButtonElement) {
  return button?.querySelector<HTMLElement>('[data-health-timeline-cell]')
}

function animatePointWave(centerIndex: number) {
  const buttons = timelineButtons()
  const centerButton = buttons[centerIndex]
  if (!centerButton) return

  const centerRow = centerButton.offsetTop
  const nextCellIndexes = new Set<number>()
  const cellsByDistance: HTMLElement[][] = [[], [], []]

  for (let distance = 0; distance <= 2; distance += 1) {
    const candidateIndexes =
      distance === 0 ? [centerIndex] : [centerIndex - distance, centerIndex + distance]

    for (const index of candidateIndexes) {
      const button = buttons[index]
      const point = points.value[index]
      const cell = timelineCell(button)
      if (
        !button ||
        !point ||
        !cell ||
        button.offsetTop !== centerRow ||
        !isInteractivePoint(point)
      ) {
        continue
      }

      nextCellIndexes.add(index)
      cellsByDistance[distance]?.push(cell)
    }
  }

  const cellsToRelease = [...wavedCellIndexes]
    .filter((index) => !nextCellIndexes.has(index))
    .map((index) => timelineCell(buttons[index]))
    .filter((cell): cell is HTMLElement => Boolean(cell))

  if (preferredMotion.value === 'reduce') {
    const cells = cellsByDistance.flat()
    gsap.set([...cells, ...cellsToRelease], { clearProps: 'transform,willChange' })
    wavedCellIndexes = nextCellIndexes
    return
  }

  animateWaveCells(cellsByDistance[0] ?? [], -6, 1.16, 0.26, 'back.out(1.45)')
  animateWaveCells(cellsByDistance[1] ?? [], -3.25, 1.08, 0.34, 'power3.out')
  animateWaveCells(cellsByDistance[2] ?? [], -1.25, 1.03, 0.42, 'power3.out')
  settleWaveCells(cellsToRelease)
  wavedCellIndexes = nextCellIndexes
}

function animateWaveCells(
  cells: HTMLElement[],
  y: number,
  scaleY: number,
  duration: number,
  ease: string,
) {
  if (cells.length === 0) return

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
  if (cells.length === 0) return

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
    .map((index) => timelineCell(buttons[index]))
    .filter((cell): cell is HTMLElement => Boolean(cell))

  if (preferredMotion.value === 'reduce') {
    gsap.set(cells, { clearProps: 'transform,willChange' })
  } else {
    settleWaveCells(cells)
  }
  wavedCellIndexes = new Set<number>()
}

function pointAccessibilityLabel(point: HealthTimelinePoint) {
  const eligibleRequests = point.successRequests + point.failedRequests
  return `${point.time}，${healthStatusMeta[point.status].label}，有效请求 ${formatHealthCount(eligibleRequests)}，可用性 ${point.reliabilityDisplay}`
}

watch(popoverOpen, (open) => {
  if (open) return

  resetPointInteraction()
})

onBeforeUnmount(() => {
  const cells = timelineButtons()
    .map((button) => timelineCell(button))
    .filter((cell): cell is HTMLElement => Boolean(cell))
  gsap.killTweensOf(cells)
})
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    :title="timeline.title"
    :description="timeline.description"
    header-collapse-at="lg"
    class="w-full"
  >
    <template #actions>
      <div class="flex w-full flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div
          class="flex max-w-full flex-wrap items-center gap-x-2 gap-y-1 text-[11px] leading-none font-[650] text-(--cp-text-muted)"
        >
          <span
            v-for="item in healthLegend"
            :key="item.status"
            class="inline-flex h-3.5 items-center gap-1 align-middle leading-none"
          >
            <span
              aria-hidden="true"
              class="block size-2 shrink-0 rounded-xs"
              :class="healthStatusMeta[item.status].cellClass"
            />
            <span class="block leading-none">{{ item.label }}</span>
          </span>
        </div>
        <div class="flex shrink-0 items-center gap-3">
          <strong
            class="font-mono text-sm leading-none font-[760] tabular-nums"
            :class="healthReliabilityValueClass(timeline.successRequests, timeline.failedRequests)"
          >
            {{ timeline.reliabilityDisplay }}
          </strong>
        </div>
      </div>
    </template>

    <template #body>
      <div class="mt-4.25">
        <BasePopover
          v-model="popoverOpen"
          trigger="hover"
          placement="top"
          :offset="12"
          width="288px"
          :anchor-element="activeAnchor"
          :disabled="!activePoint"
          animate-position
          panel-class="!p-0 text-(--cp-text-primary)"
          trigger-class="w-full"
          class="block! min-w-0 w-full"
        >
          <template #trigger>
            <div
              ref="timelineGrid"
              class="grid w-full grid-cols-48 items-end gap-x-0.5 gap-y-1 sm:grid-cols-96"
            >
              <button
                v-for="(point, pointIndex) in points"
                :key="point.time"
                data-health-timeline-point
                type="button"
                :aria-disabled="!isInteractivePoint(point)"
                :tabindex="isInteractivePoint(point) ? 0 : -1"
                :aria-expanded="isActivePoint(point)"
                aria-haspopup="dialog"
                :aria-label="pointAccessibilityLabel(point)"
                class="group relative flex h-5 w-full min-w-0.5 items-center border-0 bg-transparent p-0 outline-none"
                :class="isInteractivePoint(point) ? 'cursor-pointer' : 'cursor-default'"
                @mouseenter="activatePoint(point, pointIndex, $event)"
                @focus="activatePoint(point, pointIndex, $event)"
                @blur="closePointPopover"
                @click="activatePoint(point, pointIndex, $event)"
              >
                <span
                  aria-hidden="true"
                  data-health-timeline-cell
                  class="block h-3.5 w-full origin-bottom rounded-xs transition-[filter,box-shadow] duration-250 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none group-focus-visible:shadow-[0_0_0_2px_var(--cp-bg-surface),0_0_0_4px_var(--cp-info-border)]"
                  :class="[
                    healthStatusMeta[point.status].cellClass,
                    isActivePoint(point)
                      ? 'brightness-105 shadow-[0_5px_10px_-5px_var(--cp-shadow-sticky)]'
                      : isInteractivePoint(point)
                        ? 'group-hover:brightness-95'
                        : undefined,
                  ]"
                />
                <span
                  aria-hidden="true"
                  class="pointer-events-none absolute bottom-0 left-1/2 h-0.75 w-1.5 -translate-x-1/2 rounded-full bg-(--cp-text-primary) transition-[opacity,transform] duration-200 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
                  :class="
                    isActivePoint(point) ? 'translate-y-0 opacity-60' : 'translate-y-1 opacity-0'
                  "
                />
              </button>
            </div>
          </template>

          <HealthTimelinePointPopover v-if="activePoint" :point="activePoint" />
        </BasePopover>
      </div>
    </template>
  </BaseCard>
</template>
