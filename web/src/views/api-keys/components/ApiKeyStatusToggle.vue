<script setup lang="ts">
import { LoaderCircle } from '@lucide/vue'

defineProps<{
  apiKey: any
  loading: boolean
}>()

const emit = defineEmits<{
  toggle: [apiKey: any]
}>()
</script>

<template>
  <button
    class="inline-flex h-6 min-w-14 cursor-pointer items-center justify-center rounded-full border-0 px-2 text-[12px] leading-none font-bold"
    :class="{
      'bg-(--cp-success-bg) text-(--cp-success-text)': apiKey.enabled,
      'bg-(--cp-bg-subtle) text-(--cp-text-secondary)': !apiKey.enabled,
    }"
    :disabled="loading"
    @click="emit('toggle', apiKey)"
  >
    <LoaderCircle v-if="loading" class="mr-1 size-3 animate-spin" />
    {{ apiKey.enabled ? '已启用' : '已禁用' }}
  </button>
</template>
