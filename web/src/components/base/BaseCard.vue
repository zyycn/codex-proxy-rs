<script setup lang="ts">
import { computed } from 'vue'

type CardVariant = 'default' | 'dashboard'
type HeaderCollapseAt = 'sm' | 'lg' | 'none'

const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    padded?: boolean
    variant?: CardVariant
    title?: string
    description?: string
    headerCollapseAt?: HeaderCollapseAt
    radiusClass?: string
    shadowClass?: string
    headerClass?: string
    headerContentClass?: string
    actionsClass?: string
    bodyClass?: string
  }>(),
  {
    as: 'section',
    padded: true,
    variant: 'default',
    title: undefined,
    description: undefined,
    headerCollapseAt: 'sm',
    radiusClass: 'rounded-(--cp-card-radius)',
    shadowClass: 'shadow-(--cp-shadow-card)',
    headerClass: undefined,
    headerContentClass: '',
    actionsClass: '',
    bodyClass: '',
  },
)

const paddingClasses: Record<CardVariant, string> = {
  default: 'px-5 py-3',
  dashboard: 'px-7 py-5.5',
}

const slots = defineSlots<{
  header?: () => unknown
  title?: () => unknown
  description?: () => unknown
  actions?: () => unknown
  body?: () => unknown
  default?: () => unknown
}>()

const hasManagedHeader = computed(
  () =>
    !!props.title || !!props.description || !!slots.actions || !!slots.title || !!slots.description,
)

const managedHeaderLayoutClasses = computed(() => {
  if (props.headerCollapseAt === 'none') {
    return 'flex items-start justify-between gap-3'
  }

  if (props.headerCollapseAt === 'lg') {
    return 'flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between'
  }

  return 'flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between'
})
</script>

<template>
  <component
    :is="props.as"
    class="[--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] overflow-hidden bg-(--cp-bg-surface)"
    :class="[
      props.radiusClass,
      props.shadowClass,
      props.padded ? paddingClasses[props.variant] : undefined,
    ]"
  >
    <template v-if="$slots.header || hasManagedHeader || $slots.body">
      <header v-if="$slots.header || hasManagedHeader" class="shrink-0" :class="props.headerClass">
        <slot name="header">
          <div :class="managedHeaderLayoutClasses">
            <div class="min-w-0 pt-0.5" :class="props.headerContentClass">
              <h2
                v-if="props.title || $slots.title"
                class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)"
              >
                <slot name="title">{{ props.title }}</slot>
              </h2>
              <p
                v-if="props.description || $slots.description"
                class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)"
              >
                <slot name="description">{{ props.description }}</slot>
              </p>
            </div>

            <div v-if="$slots.actions" class="shrink-0" :class="props.actionsClass">
              <slot name="actions" />
            </div>
          </div>
        </slot>
      </header>

      <div v-if="$slots.body || $slots.default" :class="props.bodyClass">
        <slot name="body">
          <slot />
        </slot>
      </div>
    </template>

    <slot v-else />
  </component>
</template>
