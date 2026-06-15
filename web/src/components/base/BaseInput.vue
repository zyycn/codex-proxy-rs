<script setup lang="ts">
withDefaults(defineProps<{
  placeholder?: string
  disabled?: boolean
  error?: string
  size?: 'small' | 'default' | 'large'
}>(), {
  placeholder: '',
  disabled: false,
  error: undefined,
  size: 'default',
})

const model = defineModel<string>({ default: '' })
</script>

<template>
  <label class="grid gap-[7px]">
    <span
      class="inline-flex w-full items-center gap-2.5 rounded-[var(--cp-control-radius-base)] border border-transparent bg-[var(--cp-default-bg)] text-[var(--cp-text-primary)] transition focus-within:border-[var(--cp-info-border)] focus-within:bg-[var(--cp-bg-surface)] focus-within:shadow-[var(--cp-shadow-control)]"
      :class="[
        size === 'large' ? 'h-10 px-4' : size === 'small' ? 'h-6 rounded-[var(--cp-control-radius-small)] px-2' : 'h-8 px-3',
        error ? 'border-[var(--cp-danger-border)] bg-[var(--cp-danger-bg)]' : '',
      ]"
    >
      <span v-if="$slots.prefix" class="inline-flex text-[var(--cp-text-muted)]">
        <slot name="prefix" />
      </span>
      <input
        v-model="model"
        class="min-w-0 flex-1 border-0 bg-transparent text-[13px] font-semibold text-[var(--cp-text-primary)] outline-0 placeholder:text-[var(--cp-text-muted)] disabled:text-[var(--cp-disabled-text)]"
        :placeholder="placeholder"
        :disabled="disabled"
      />
    </span>
    <span v-if="error" class="text-xs font-semibold text-[var(--cp-danger-text)]">{{ error }}</span>
  </label>
</template>
