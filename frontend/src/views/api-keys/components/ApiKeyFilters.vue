<script setup lang="ts">
import { Plus, Search, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineProps<{
  batchDeleting: boolean
  selectedCount: number
}>()

const search = defineModel<string>('search', { required: true })

const emit = defineEmits<{
  create: []
  deleteSelected: []
}>()
</script>

<template>
  <div class="flex flex-wrap items-center justify-between gap-3" aria-label="API Key 筛选">
    <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
      <BaseInput
        v-model="search"
        placeholder="搜索名称、标签或 ID"
        class="min-w-64 flex-1 sm:max-w-96"
      >
        <template #prefix>
          <Search class="size-4.5 text-(--cp-text-tertiary)" />
        </template>
      </BaseInput>

      <BaseButton
        v-if="selectedCount > 0"
        variant="danger"
        :disabled="batchDeleting"
        @click="emit('deleteSelected')"
      >
        <template #icon>
          <Trash2 class="size-4" />
        </template>
        删除选中 ({{ selectedCount }})
      </BaseButton>
    </div>

    <div class="flex shrink-0 items-center gap-2">
      <BaseButton variant="primary" @click="emit('create')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        创建 API Key
      </BaseButton>
    </div>
  </div>
</template>
