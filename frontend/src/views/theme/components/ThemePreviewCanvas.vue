<script setup lang="ts">
import type { CSSProperties } from 'vue'
import type { ThemeName } from '@/theme'
import { Maximize2, Minus, Move, Plus } from '@lucide/vue'
import { computed, useTemplateRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'

import { useThemePreviewCanvas } from '../composables/useThemePreviewCanvas'
import ThemePreviewScope from './ThemePreviewScope.vue'

defineProps<{
  theme: ThemeName
  style: CSSProperties
}>()

const viewport = useTemplateRef<HTMLElement>('viewport')
const stage = useTemplateRef<HTMLElement>('stage')
const boardWidth = 1600
const boardHeight = 1808
const {
  canZoomIn,
  canZoomOut,
  dragging,
  fitToViewport,
  resetScale,
  scale,
  scalePercent,
  zoomIn,
  zoomOut,
} = useThemePreviewCanvas({ viewport, stage, boardWidth, boardHeight })

const stageStyle = computed<CSSProperties>(() => ({
  width: `${boardWidth * scale.value}px`,
  height: `${boardHeight * scale.value}px`,
}))
const boardStyle = computed<CSSProperties>(() => ({
  width: `${boardWidth}px`,
  height: `${boardHeight}px`,
  zoom: scale.value,
}))
</script>

<template>
  <div
    ref="viewport"
    class="theme-preview-canvas relative min-h-0 overflow-hidden rounded-cp-lg bg-cp-bg-layout shadow-cp-secondary select-none"
    :class="dragging ? 'cursor-grabbing' : 'cursor-grab'"
    role="application"
    tabindex="0"
    aria-label="首页主题预览画板"
  >
    <div
      ref="stage"
      class="pointer-events-none absolute top-0 left-0 z-0 overflow-hidden"
      data-theme-preview-stage
      :data-scale="scalePercent"
      :style="stageStyle"
    >
      <ThemePreviewScope
        :theme="theme"
        :style="style"
      >
        <div
          class="overflow-hidden bg-cp-bg-layout p-6"
          data-theme-preview-board
          :style="boardStyle"
        >
          <slot />
        </div>
      </ThemePreviewScope>
    </div>

    <div
      class="pointer-events-none absolute top-3 left-3 z-10 inline-flex items-center gap-2 rounded-cp bg-cp-bg-container/92 px-2.5 py-2 text-[10px] font-emphasis text-cp-text-secondary shadow-cp-tertiary backdrop-blur-sm"
    >
      <Move class="size-3.5 text-cp-primary-text" />
      拖动空白处平移 · 滚轮缩放
    </div>

    <div
      class="absolute top-3 right-3 z-10 flex items-center gap-1 rounded-cp-lg bg-cp-bg-container/94 p-1.5 shadow-cp-popup backdrop-blur-md"
      data-canvas-control
      @pointerdown.stop
      @dblclick.stop
      @wheel.stop
    >
      <BaseIconButton
        label="缩小画板"
        size="sm"
        variant="ghost"
        :disabled="!canZoomOut"
        @click="zoomOut"
      >
        <Minus class="size-3.5" />
      </BaseIconButton>
      <BaseButton
        size="sm"
        variant="ghost"
        class="min-w-14 px-2! font-mono tabular-nums"
        title="恢复 100%"
        @click="resetScale"
      >
        {{ scalePercent }}%
      </BaseButton>
      <BaseIconButton
        label="放大画板"
        size="sm"
        variant="ghost"
        :disabled="!canZoomIn"
        @click="zoomIn"
      >
        <Plus class="size-3.5" />
      </BaseIconButton>
      <span class="mx-0.5 h-4 w-px bg-cp-fill-secondary" aria-hidden="true" />
      <BaseIconButton label="适应画板" size="sm" variant="ghost" @click="fitToViewport()">
        <Maximize2 class="size-3.5" />
      </BaseIconButton>
    </div>
  </div>
</template>

<style scoped>
.theme-preview-canvas {
  background-image: radial-gradient(
    circle,
    color-mix(in srgb, var(--cp-color-text-quaternary) 24%, transparent) 1px,
    transparent 1px
  );
  background-size: 18px 18px;
}
</style>
