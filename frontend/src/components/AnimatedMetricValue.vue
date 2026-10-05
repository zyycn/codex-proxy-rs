<script setup lang="ts">
import { TransitionPresets, usePreferredReducedMotion, useTransition } from '@vueuse/core'
import { computed } from 'vue'

type MetricValueFormatter = (value: number) => string

const props = withDefaults(
  defineProps<{
    value: string
    rawValue?: number | null
    formatter?: MetricValueFormatter
    duration?: number
  }>(),
  {
    rawValue: null,
    duration: 520,
  },
)

const preferredMotion = usePreferredReducedMotion()
const hasAnimatedValue = computed(
  () =>
    typeof props.rawValue === 'number'
    && Number.isFinite(props.rawValue)
    && Boolean(props.formatter),
)
const sourceValue = computed(() => (hasAnimatedValue.value ? (props.rawValue ?? 0) : 0))
const disabled = computed(() => preferredMotion.value === 'reduce' || !hasAnimatedValue.value)
const animatedValue = useTransition(sourceValue, {
  duration: computed(() => props.duration),
  easing: TransitionPresets.easeOutCubic,
  disabled,
})

const displayValue = computed(() =>
  hasAnimatedValue.value && props.formatter ? props.formatter(animatedValue.value) : props.value,
)
</script>

<template>
  <span>{{ displayValue }}</span>
</template>
