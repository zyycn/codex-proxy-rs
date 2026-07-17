<script setup lang="ts">
import { Plus, Search, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineProps<{
  batchDeleting: boolean
  selectedCount: number
}>()

const emit = defineEmits<{
  create: []
  deleteSelected: []
}>()

const search = defineModel<string>('search', { required: true })
</script>

<template>
  <div
    class="flex w-full flex-col gap-3 md:flex-row md:flex-wrap md:items-center"
    role="group"
    aria-label="API Key 筛选与操作"
  >
    <div class="min-w-0 w-full md:w-96 md:flex-none">
      <BaseInput v-model="search" placeholder="搜索名称、标签或 ID" class="w-full">
        <template #prefix>
          <Search class="size-4.5 text-(--cp-text-tertiary)" />
        </template>
      </BaseInput>
    </div>

    <div class="flex shrink-0 self-end items-center justify-end gap-2 md:ml-auto">
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
      <BaseButton variant="primary" @click="emit('create')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        创建 API Key
      </BaseButton>
    </div>
  </div>
</template>
