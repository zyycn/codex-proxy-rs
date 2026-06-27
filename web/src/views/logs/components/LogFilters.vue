<script setup lang="ts">
import { RefreshCw, Search, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

defineProps<{
  levelOptions: any[]
  refreshing: boolean
  loading: boolean
  clearing: boolean
}>()

const search = defineModel<string>('search', { required: true })
const level = defineModel<string>('level', { required: true })

const emit = defineEmits<{
  refresh: []
  clear: []
}>()
</script>

<template>
  <div class="flex flex-wrap items-center justify-between gap-3" aria-label="日志筛选">
    <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
      <BaseInput
        v-model="search"
        placeholder="搜索消息、请求 ID 或路由"
        class="min-w-64 flex-1 sm:max-w-96"
      >
        <template #prefix>
          <Search class="size-4.5 text-(--cp-text-tertiary)" />
        </template>
      </BaseInput>

      <BaseSelect v-model="level" :options="levelOptions" class="w-34" />
    </div>

    <div class="flex shrink-0 items-center gap-2">
      <BaseButton
        icon-only
        variant="ghost"
        size="md"
        label="刷新日志"
        :disabled="loading || refreshing"
        @click="emit('refresh')"
      >
        <RefreshCw class="size-4.5" :class="refreshing ? 'animate-spin' : undefined" />
      </BaseButton>

      <BaseButton variant="danger" :disabled="clearing" @click="emit('clear')">
        <template #icon>
          <Trash2 class="size-4" />
        </template>
        清空日志
      </BaseButton>
    </div>
  </div>
</template>
