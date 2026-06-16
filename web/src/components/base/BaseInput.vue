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
      class="inline-flex w-full items-center gap-2.5 rounded-(--cp-control-radius-base) border border-transparent bg-(--cp-default-bg) text-(--cp-text-primary) transition focus-within:border-(--cp-info-border) focus-within:bg-(--cp-bg-surface) focus-within:shadow-(--cp-shadow-control)"
      :class="[
        size === 'large' ? 'h-10 px-4' : size === 'small' ? 'h-6 rounded-(--cp-control-radius-small) px-2' : 'h-8 px-3',
        error ? 'border-(--cp-danger-border) bg-(--cp-danger-bg)' : '',
      ]"
    >
      <span v-if="$slots.prefix" class="inline-flex text-(--cp-text-muted)">
        <slot name="prefix" />
      </span>
      <input
        v-model="model"
        class="min-w-0 flex-1 border-0 bg-transparent text-[13px] font-semibold text-(--cp-text-primary) outline-0 placeholder:text-(--cp-text-muted) disabled:text-(--cp-disabled-text)"
        :placeholder="placeholder"
        :disabled="disabled"
      />
    </span>
    <span v-if="error" class="text-xs font-semibold text-(--cp-danger-text)">{{ error }}</span>
  </label>
</template>
