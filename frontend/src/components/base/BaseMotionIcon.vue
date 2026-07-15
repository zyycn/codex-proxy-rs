<script setup lang="ts">
import { usePreferredReducedMotion } from '@vueuse/core'
import { gsap } from 'gsap'
import { onBeforeUnmount, useTemplateRef, watch } from 'vue'

type MotionIconVariant = 'random' | 'brand'
type MotionPhase = 'enter' | 'leave'
type MotionTimeline = ReturnType<typeof gsap.timeline>
type ReactionBuilder = (root: HTMLElement, glyph: HTMLElement) => MotionTimeline

interface MotionReaction {
  enter: ReactionBuilder
  leave: ReactionBuilder
}

const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    variant?: MotionIconVariant
  }>(),
  {
    as: 'span',
    variant: 'random',
  },
)

const preferredMotion = usePreferredReducedMotion()
const icon = useTemplateRef<HTMLElement>('icon')
const glyph = useTemplateRef<HTMLElement>('glyph')
const randomReactions: MotionReaction[] = [
  { enter: liftAndBloomEnter, leave: liftAndBloomLeave },
  { enter: lookAroundEnter, leave: lookAroundLeave },
  { enter: doubleTapEnter, leave: doubleTapLeave },
  { enter: counterSwingEnter, leave: counterSwingLeave },
  { enter: shortOrbitEnter, leave: shortOrbitLeave },
]
const brandReaction: MotionReaction = {
  enter: brandGreetingEnter,
  leave: brandGreetingLeave,
}
let previousReactionIndex = -1
let activeReaction: MotionReaction | undefined
let activeTimeline: MotionTimeline | undefined
let isPointerInside = false

function motionTargets() {
  return [icon.value, glyph.value].filter((target): target is HTMLElement => Boolean(target))
}

function clearMotionStyles(element: HTMLElement) {
  gsap.set(element, { clearProps: 'transform,transformOrigin,willChange' })
}

function stopActiveTimeline(targets: HTMLElement[]) {
  activeTimeline?.kill()
  activeTimeline = undefined
  gsap.killTweensOf(targets)
}

function resetMotion() {
  const targets = motionTargets()
  stopActiveTimeline(targets)
  activeReaction = undefined
  for (const target of targets) clearMotionStyles(target)
}

function createMotionTimeline(root: HTMLElement, glyphElement: HTMLElement, phase: MotionPhase) {
  const targets = [root, glyphElement]
  stopActiveTimeline(targets)
  gsap.set(targets, { willChange: 'transform' })

  let timeline: MotionTimeline
  timeline = gsap.timeline({
    defaults: { ease: 'power2.out' },
    onComplete: () => {
      if (activeTimeline !== timeline) return
      activeTimeline = undefined

      if (phase === 'leave' && !isPointerInside) {
        activeReaction = undefined
        for (const target of targets) clearMotionStyles(target)
        return
      }

      gsap.set(targets, { clearProps: 'willChange' })
    },
  })
  activeTimeline = timeline
  return timeline
}

function nextRandomReaction() {
  const index =
    previousReactionIndex < 0
      ? Math.floor(Math.random() * randomReactions.length)
      : (previousReactionIndex + 1 + Math.floor(Math.random() * (randomReactions.length - 1))) %
        randomReactions.length
  previousReactionIndex = index
  return randomReactions[index]!
}

function liftAndBloomEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { y: -3, scale: 1.055, duration: 0.22 }, 0)
    .to(
      glyphElement,
      { y: -0.5, rotation: -4, scale: 1.13, duration: 0.26, ease: 'back.out(2.1)' },
      0.02,
    )
  return timeline
}

function liftAndBloomLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(glyphElement, { y: 0, rotation: 0, scale: 1, duration: 0.24, ease: 'back.out(1.6)' }, 0)
    .to(root, { y: 0, scale: 1, duration: 0.22, ease: 'power3.out' }, 0.02)
  return timeline
}

function lookAroundEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { y: -1, scale: 1.025, duration: 0.2 }, 0)
    .to(glyphElement, { x: -2.5, rotation: -7, duration: 0.13 }, 0)
    .to(glyphElement, { x: 2.5, rotation: 7, duration: 0.17, ease: 'power2.inOut' }, 0.13)
    .to(
      glyphElement,
      { x: 1, rotation: 3, scale: 1.06, duration: 0.16, ease: 'back.out(1.7)' },
      0.3,
    )
  return timeline
}

function lookAroundLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(glyphElement, { x: 0, rotation: 0, scale: 1, duration: 0.24, ease: 'back.out(1.7)' }, 0)
    .to(root, { y: 0, scale: 1, duration: 0.22, ease: 'power3.out' }, 0.02)
  return timeline
}

function doubleTapEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { y: 1, scale: 0.965, duration: 0.09, ease: 'power2.in' }, 0)
    .to(glyphElement, { y: 1.5, scale: 0.92, duration: 0.09, ease: 'power2.in' }, 0)
    .to(root, { y: -3, scale: 1.055, duration: 0.17, ease: 'back.out(2.2)' }, 0.09)
    .to(glyphElement, { y: -2, scale: 1.12, duration: 0.19, ease: 'back.out(2.4)' }, 0.09)
  return timeline
}

function doubleTapLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(root, { y: 0.5, scale: 0.985, duration: 0.1, ease: 'power2.in' }, 0)
    .to(glyphElement, { y: 0.75, scale: 0.97, duration: 0.1, ease: 'power2.in' }, 0)
    .to(root, { y: 0, scale: 1, duration: 0.17, ease: 'back.out(1.8)' }, 0.1)
    .to(glyphElement, { y: 0, scale: 1, duration: 0.18, ease: 'back.out(1.9)' }, 0.1)
  return timeline
}

function counterSwingEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { x: -1.5, y: -1.5, rotation: -4, scale: 1.035, duration: 0.14 }, 0)
    .to(glyphElement, { x: 2, rotation: 8, duration: 0.14 }, 0)
    .to(root, { x: 1, y: -1, rotation: 2, duration: 0.15, ease: 'power2.inOut' }, 0.14)
    .to(glyphElement, { x: -1, rotation: -5, duration: 0.15, ease: 'power2.inOut' }, 0.14)
  return timeline
}

function counterSwingLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(root, { x: 0, y: 0, rotation: 0, scale: 1, duration: 0.23, ease: 'back.out(1.6)' }, 0)
    .to(glyphElement, { x: 0, rotation: 0, duration: 0.21, ease: 'back.out(1.8)' }, 0.02)
  return timeline
}

function shortOrbitEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { y: -1.5, scale: 1.025, duration: 0.18 }, 0)
    .to(glyphElement, { x: 2.5, y: -2, rotation: 5, duration: 0.13 }, 0)
    .to(glyphElement, { x: -2, y: -0.5, rotation: -5, duration: 0.15 }, 0.13)
    .to(
      glyphElement,
      { x: 0.75, y: -1, rotation: 2, scale: 1.08, duration: 0.18, ease: 'back.out(1.8)' },
      0.28,
    )
  return timeline
}

function shortOrbitLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(glyphElement, {
      x: 0,
      y: 0,
      rotation: 0,
      scale: 1,
      duration: 0.24,
      ease: 'back.out(1.7)',
    })
    .to(root, { y: 0, scale: 1, duration: 0.22, ease: 'power3.out' }, 0.02)
  return timeline
}

function brandGreetingEnter(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'enter')
  timeline
    .to(root, { scale: 0.965, duration: 0.09, ease: 'power2.in' }, 0)
    .to(glyphElement, { y: 1, scale: 0.92, duration: 0.09, ease: 'power2.in' }, 0)
    .to(root, { scale: 1, duration: 0.18, ease: 'back.out(2.4)' }, 0.09)
    .to(
      glyphElement,
      { y: -1.5, rotation: -5, scale: 1.28, duration: 0.22, ease: 'back.out(3)' },
      0.08,
    )
    .to(glyphElement, { x: 1, rotation: 4, scale: 1.2, duration: 0.13, ease: 'power1.inOut' }, 0.3)
    .to(
      glyphElement,
      { x: -0.5, rotation: -2, scale: 1.24, duration: 0.11, ease: 'power1.inOut' },
      0.43,
    )
  return timeline
}

function brandGreetingLeave(root: HTMLElement, glyphElement: HTMLElement) {
  const timeline = createMotionTimeline(root, glyphElement, 'leave')
  timeline
    .to(glyphElement, {
      x: 0.35,
      y: -0.5,
      rotation: 1,
      scale: 1.08,
      duration: 0.14,
      ease: 'power2.in',
    })
    .to(root, { scale: 0.985, duration: 0.14, ease: 'power2.in' }, 0)
    .to(
      glyphElement,
      { x: 0, y: 0, rotation: 0, scale: 1, duration: 0.2, ease: 'back.out(1.8)' },
      0.12,
    )
    .to(root, { scale: 1, duration: 0.2, ease: 'back.out(1.7)' }, 0.12)
  return timeline
}

function playEnterReaction() {
  isPointerInside = true
  if (preferredMotion.value === 'reduce') {
    resetMotion()
    return
  }

  const root = icon.value
  const glyphElement = glyph.value
  if (!root || !glyphElement) return

  activeReaction = props.variant === 'brand' ? brandReaction : nextRandomReaction()
  activeReaction.enter(root, glyphElement)
}

function playLeaveReaction() {
  isPointerInside = false
  if (preferredMotion.value === 'reduce') {
    resetMotion()
    return
  }

  const root = icon.value
  const glyphElement = glyph.value
  if (!root || !glyphElement || !activeReaction) {
    resetMotion()
    return
  }

  activeReaction.leave(root, glyphElement)
}

watch(preferredMotion, (motion) => {
  if (motion === 'reduce') resetMotion()
})

onBeforeUnmount(resetMotion)
</script>

<template>
  <component
    :is="props.as"
    ref="icon"
    @pointerenter="playEnterReaction"
    @pointerleave="playLeaveReaction"
  >
    <span ref="glyph" class="inline-flex items-center justify-center">
      <slot />
    </span>
  </component>
</template>
