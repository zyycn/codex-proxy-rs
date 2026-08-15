<script setup lang="ts">
import { RefreshCw, Search } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineProps<{
  refreshing: boolean
  loading: boolean
}>()

const emit = defineEmits<{
  refresh: []
}>()

const search = defineModel<string>('search', { required: true })
</script>

<template>
  <div class="flex w-full items-center gap-3" role="group" aria-label="使用记录筛选与操作">
    <div class="min-w-0 flex-1 sm:w-96 sm:flex-none">
      <BaseInput v-model="search" placeholder="搜索请求 ID、端点、模型或消息" class="w-full">
        <template #prefix>
          <Search class="size-4.5 text-cp-tertiary" />
        </template>
      </BaseInput>
    </div>

    <div class="ml-auto flex shrink-0 items-center justify-end">
      <BaseIconButton
        variant="ghost"
        size="md"
        label="刷新使用记录"
        :loading="refreshing"
        :disabled="loading || refreshing"
        @click="emit('refresh')"
      >
        <template #loading>
          <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
        </template>
        <RefreshCw class="size-4.5" />
      </BaseIconButton>
    </div>
  </div>
</template>
