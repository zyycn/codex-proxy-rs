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
  <div class="flex flex-wrap items-center justify-between gap-3" aria-label="使用记录筛选">
    <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
      <BaseInput
        v-model="search"
        placeholder="搜索请求 ID、端点、模型或消息"
        class="min-w-64 flex-1 sm:max-w-96"
      >
        <template #prefix>
          <Search class="size-4.5 text-(--cp-text-tertiary)" />
        </template>
      </BaseInput>
    </div>

    <div class="flex shrink-0 items-center gap-2">
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
