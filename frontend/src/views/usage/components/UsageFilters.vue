<script setup lang="ts">
import { RefreshCw, Search } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineProps<{
  refreshing: boolean
  loading: boolean
}>()

const search = defineModel<string>('search', { required: true })

const emit = defineEmits<{
  refresh: []
}>()
</script>

<template>
  <div
    class="flex w-full flex-col gap-3 md:flex-row md:flex-wrap md:items-center"
    role="group"
    aria-label="使用记录筛选与操作"
  >
    <div class="min-w-0 w-full md:w-96 md:flex-none">
      <BaseInput v-model="search" placeholder="搜索请求 ID、端点、模型或消息" class="w-full">
        <template #prefix>
          <Search class="size-4.5 text-(--cp-text-tertiary)" />
        </template>
      </BaseInput>
    </div>

    <div class="flex shrink-0 self-end items-center justify-end gap-2 md:ml-auto">
      <BaseButton
        icon-only
        variant="ghost"
        size="md"
        label="刷新使用记录"
        :disabled="loading || refreshing"
        @click="emit('refresh')"
      >
        <RefreshCw class="size-4.5" :class="refreshing ? 'animate-spin' : undefined" />
      </BaseButton>
    </div>
  </div>
</template>
