<script setup lang="ts">
import { computed } from 'vue'

type SkeletonShape = 'text' | 'block' | 'circle'

const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    shape?: SkeletonShape
    animated?: boolean
  }>(),
  {
    as: 'span',
    shape: 'block',
    animated: true,
  },
)

const shapeClasses: Record<SkeletonShape, string> = {
  text: 'h-3 rounded-full',
  block: 'rounded-cp',
  circle: 'rounded-full',
}

const classes = computed(() => [
  'block bg-cp-fill-secondary',
  shapeClasses[props.shape],
  props.animated ? 'motion-safe:animate-cp-skeleton motion-reduce:animate-none' : undefined,
])
</script>

<template>
  <component
    :is="as"
    aria-hidden="true"
    :class="classes"
  />
</template>
