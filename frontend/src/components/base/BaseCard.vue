<script setup lang="ts">
import { computed } from 'vue'

type CardPadding = 'none' | 'compact' | 'default'
const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    padding?: CardPadding
    title?: string
    description?: string
  }>(),
  {
    as: 'section',
    padding: 'default',
    title: undefined,
    description: undefined,
  },
)

const slots = defineSlots<{
  header?: () => unknown
  title?: () => unknown
  description?: () => unknown
  actions?: () => unknown
  body?: () => unknown
  default?: () => unknown
}>()

const paddingClasses: Record<CardPadding, string | undefined> = {
  none: undefined,
  compact: 'p-4',
  default: 'p-5.5',
}

const hasManagedHeader = computed(
  () => Boolean(props.title || props.description || slots.actions || slots.title || slots.description),
)
const hasHeader = computed(() => Boolean(slots.header || hasManagedHeader.value))
const contentClasses = computed(() => [
  hasHeader.value ? 'mt-4' : undefined,
  'flex min-h-0 min-w-0 flex-1 flex-col',
])
</script>

<template>
  <component
    :is="props.as"
    class="[--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] overflow-hidden rounded-cp-surface bg-cp-surface shadow-cp-card"
    :class="[paddingClasses[padding], hasHeader ? 'flex min-h-0 flex-col' : undefined]"
  >
    <template v-if="$slots.header || hasManagedHeader || $slots.body">
      <header v-if="$slots.header || hasManagedHeader" class="shrink-0">
        <slot name="header">
          <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div class="min-w-0 pt-0.5">
              <h2
                v-if="props.title || $slots.title"
                class="m-0 text-xl leading-[1.15] font-heavy text-cp-primary text-balance"
              >
                <slot name="title">
                  {{ props.title }}
                </slot>
              </h2>
              <p
                v-if="props.description || $slots.description"
                class="mt-1.75 mb-0 text-[13px] leading-[1.3] font-emphasis text-cp-secondary text-pretty"
              >
                <slot name="description">
                  {{ props.description }}
                </slot>
              </p>
            </div>
            <div v-if="$slots.actions" class="shrink-0">
              <slot name="actions" />
            </div>
          </div>
        </slot>
      </header>

      <div v-if="$slots.body || $slots.default" :class="contentClasses">
        <slot name="body">
          <slot />
        </slot>
      </div>
    </template>

    <slot v-else />
  </component>
</template>
