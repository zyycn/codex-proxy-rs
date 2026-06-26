<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    padded?: boolean
    radiusClass?: string
    shadowClass?: string
    headerClass?: string
    bodyClass?: string
  }>(),
  {
    as: 'section',
    padded: true,
    radiusClass: 'rounded-(--cp-card-radius)',
    shadowClass: 'shadow-(--cp-shadow-card)',
    headerClass: 'px-6 pt-6 pb-3 md:px-7',
    bodyClass: '',
  },
)
</script>

<template>
  <component
    :is="props.as"
    class="[--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] overflow-hidden bg-(--cp-bg-surface)"
    :class="[
      props.radiusClass,
      props.shadowClass,
      !$slots.header && !$slots.body && props.padded ? 'p-6 md:px-7' : undefined,
    ]"
  >
    <template v-if="$slots.header || $slots.body">
      <header v-if="$slots.header" class="shrink-0" :class="props.headerClass">
        <slot name="header" />
      </header>

      <div
        v-if="$slots.body || $slots.default"
        :class="[
          props.padded ? ($slots.header ? 'px-6 pb-6 md:px-7' : 'p-6 md:px-7') : undefined,
          props.bodyClass,
        ]"
      >
        <slot name="body">
          <slot />
        </slot>
      </div>
    </template>

    <slot v-else />
  </component>
</template>
