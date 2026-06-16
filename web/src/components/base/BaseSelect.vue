<script setup lang="ts">
import { ChevronDown } from '@lucide/vue'

interface SelectOption {
  label: string
  value: string
}

defineProps<{
  options: SelectOption[]
  disabled?: boolean
}>()

const model = defineModel<string>({ required: true })
</script>

<template>
  <label class="relative inline-flex h-8 w-full rounded-(--cp-control-radius-base) bg-(--cp-default-bg)">
    <select
      v-model="model"
      class="h-full w-full appearance-none rounded-[inherit] border-0 bg-transparent pr-8 pl-3 text-[13px] font-semibold text-(--cp-text-primary) outline-0"
      :disabled="disabled"
    >
      <option v-for="option in options" :key="option.value" :value="option.value">
        {{ option.label }}
      </option>
    </select>
    <ChevronDown class="pointer-events-none absolute top-2 right-3 text-(--cp-text-secondary)" :size="16" />
  </label>
</template>
