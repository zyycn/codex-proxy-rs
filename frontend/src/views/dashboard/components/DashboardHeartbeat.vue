<script setup lang="ts">
import { usePreferredReducedMotion } from '@vueuse/core'
import { gsap } from 'gsap'
import { computed, nextTick, onBeforeUnmount, useTemplateRef, watch } from 'vue'

const props = withDefaults(
  defineProps<{
    updatedAt?: string
  }>(),
  {
    updatedAt: '',
  },
)

const preferredMotion = usePreferredReducedMotion()
const heartbeatDot = useTemplateRef<HTMLElement>('heartbeatDot')
const heartbeatRing = useTemplateRef<HTMLElement>('heartbeatRing')
const statusLabel = computed(() =>
  props.updatedAt ? `自动刷新 30s，最近刷新 ${props.updatedAt}` : '自动刷新 30s，等待首次刷新',
)

function pulse() {
  const dot = heartbeatDot.value
  const ring = heartbeatRing.value
  if (!dot || !ring || preferredMotion.value === 'reduce') return

  gsap.killTweensOf([dot, ring])
  gsap.fromTo(
    dot,
    { scale: 0.72, opacity: 0.55 },
    {
      scale: 1,
      opacity: 1,
      duration: 0.42,
      ease: 'back.out(2.2)',
      clearProps: 'transform,opacity',
    },
  )
  gsap.fromTo(
    ring,
    { scale: 0.55, opacity: 0.34 },
    {
      scale: 2.65,
      opacity: 0,
      duration: 0.65,
      ease: 'power2.out',
      clearProps: 'transform,opacity',
    },
  )
}

watch(
  () => props.updatedAt,
  async (updatedAt, previousUpdatedAt) => {
    if (!updatedAt || updatedAt === previousUpdatedAt) return
    await nextTick()
    pulse()
  },
)

onBeforeUnmount(() => {
  gsap.killTweensOf([heartbeatDot.value, heartbeatRing.value])
})
</script>

<template>
  <span class="inline-flex items-center gap-1.75" :aria-label="statusLabel" :title="statusLabel">
    <span
      aria-hidden="true"
      class="relative inline-flex size-2 shrink-0 items-center justify-center"
    >
      <span ref="heartbeatRing" class="absolute size-2 rounded-full bg-(--cp-success) opacity-0" />
      <span ref="heartbeatDot" class="relative size-1.5 rounded-full bg-(--cp-success)" />
    </span>
    <span aria-hidden="true">自动刷新 30s</span>
  </span>
</template>
