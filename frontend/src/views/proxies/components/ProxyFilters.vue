<script setup lang="ts">
import { Plus, Search } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const emit = defineEmits<{
  create: []
}>()
const search = defineModel<string>('search', { required: true })
</script>

<template>
  <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
    <BaseInput
      v-model="search"
      class="sm:w-80"
      aria-label="搜索代理"
      placeholder="搜索代理名称..."
    >
      <template #prefix>
        <Search class="size-4.5 text-cp-text-tertiary" />
      </template>
    </BaseInput>
    <div class="flex shrink-0 items-center justify-end gap-2 sm:ml-auto">
      <BaseButton variant="primary" @click="emit('create')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        添加代理
      </BaseButton>
    </div>
  </div>
</template>
