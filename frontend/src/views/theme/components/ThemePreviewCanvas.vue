<script setup lang="ts">
import type { ThemeName } from '@codex-proxy/ui/theme'

import type { CSSProperties } from 'vue'
import { ZButton, ZIconButton } from '@codex-proxy/ui'
import { Maximize2, Minus, Move, Plus } from '@lucide/vue'

import { computed, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'

import { useThemePreviewCanvas } from '../composables/useThemePreviewCanvas'
import ThemePreviewScope from './ThemePreviewScope.vue'

defineProps<{
  theme: ThemeName
  style: CSSProperties
}>()

const { t } = useI18n({ useScope: 'global' })

const viewport = useTemplateRef<HTMLElement>('viewport')
const stage = useTemplateRef<HTMLElement>('stage')
const boardWidth = 1600
const boardHeight = 1808
const { canZoomIn, canZoomOut, dragging, fitToViewport, resetScale, scale, scalePercent, zoomIn, zoomOut }
  = useThemePreviewCanvas({
    viewport,
    stage,
    boardWidth,
    boardHeight,
  })

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
    class="@container/preview relative min-h-0 overflow-hidden rounded-cp-lg bg-cp-bg-layout bg-[radial-gradient(circle,color-mix(in_srgb,var(--cp-color-text-quaternary)_24%,transparent)_1px,transparent_1px)] bg-size-[18px_18px] shadow-cp-secondary select-none"
    :class="dragging ? 'cursor-grabbing' : 'cursor-grab'"
    role="application"
    tabindex="0"
    :aria-label="t('appearance.preview.canvas.homeThemePreviewArtboard')"
  >
    <div
      ref="stage"
      class="pointer-events-none absolute top-0 left-0 z-0 overflow-hidden"
      data-theme-preview-stage
      :data-scale="scalePercent"
      :style="stageStyle"
    >
      <ThemePreviewScope :theme="theme" :style="style">
        <div class="overflow-hidden bg-cp-bg-layout p-6" data-theme-preview-board :style="boardStyle">
          <slot />
        </div>
      </ThemePreviewScope>
    </div>

    <div
      class="pointer-events-none absolute top-3 left-3 z-10 hidden items-center gap-2 rounded-cp bg-cp-bg-container/92 px-2.5 py-2 text-[10px] font-emphasis text-cp-text-secondary shadow-cp-tertiary backdrop-blur-sm @min-[36rem]/preview:inline-flex"
    >
      <Move class="size-3.5 text-cp-primary-text" />
      {{ t('appearance.preview.canvas.dragBlankSpaceToPanUseScroll') }}
    </div>

    <div
      class="absolute top-3 right-3 z-10 flex items-center gap-1 rounded-cp-lg bg-cp-bg-container/94 p-1.5 shadow-cp-popup backdrop-blur-md"
      data-canvas-control
      @pointerdown.stop
      @dblclick.stop
      @wheel.stop
    >
      <ZIconButton :aria-label="t('appearance.preview.canvas.reduceArtboard')" size="small" :disabled="!canZoomOut" @click="zoomOut">
        <Minus class="size-3.5" />
      </ZIconButton>
      <ZButton
        size="small" variant="text"
        class="min-w-14 px-2! font-mono tabular-nums"
        :title="t('appearance.preview.canvas.recovery')"
        @click="resetScale"
      >
        {{ scalePercent }}%
      </ZButton>
      <ZIconButton :aria-label="t('appearance.preview.canvas.enlargeArtboard')" size="small" :disabled="!canZoomIn" @click="zoomIn">
        <Plus class="size-3.5" />
      </ZIconButton>
      <span class="mx-0.5 h-4 w-px bg-cp-fill-secondary" />
      <ZIconButton :aria-label="t('appearance.preview.canvas.adaptToArtboard')" size="small" @click="fitToViewport()">
        <Maximize2 class="size-3.5" />
      </ZIconButton>
    </div>
  </div>
</template>
