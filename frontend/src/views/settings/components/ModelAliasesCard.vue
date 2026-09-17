<script setup lang="ts">
import { Plus, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'

withDefaults(defineProps<{
  mappings: Array<{ requestedModel: string, upstreamModel: string }>
  loading?: boolean
  error?: string
}>(), {
  loading: false,
  error: '',
})

const emit = defineEmits<{
  addMapping: []
  updateMapping: [index: number, key: 'requestedModel' | 'upstreamModel', value: string]
  removeMapping: [index: number]
}>()
</script>

<template>
  <BaseCard
    title="模型映射"
    description="配置请求模型与上游模型的映射关系"
  >
    <div class="grid gap-4">
      <div class="flex flex-wrap items-center gap-3">
        <BaseButton variant="secondary" :disabled="loading" @click="emit('addMapping')">
          <template #icon>
            <Plus class="size-4" />
          </template>
          添加映射
        </BaseButton>
        <span v-if="error" class="text-xs font-emphasis text-cp-error-text">{{ error }}</span>
      </div>

      <div
        v-if="loading"
        class="rounded-cp bg-cp-fill-quaternary px-4 py-4 text-cp font-emphasis text-cp-text-quaternary"
      >
        正在加载模型映射...
      </div>
      <div v-else-if="mappings.length > 0" class="grid max-w-6xl gap-3">
        <div
          v-for="(row, index) in mappings"
          :key="index"
          class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-cp-card bg-cp-fill-quaternary p-3 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto]"
        >
          <BaseInput
            :model-value="row.requestedModel"
            placeholder="请求模型"
            aria-label="请求模型"
            @update:model-value="emit('updateMapping', index, 'requestedModel', $event)"
          />
          <span class="hidden text-cp-text-quaternary sm:block" aria-hidden="true">→</span>
          <BaseInput
            class="col-start-1 row-start-2 sm:col-start-auto sm:row-start-auto"
            :model-value="row.upstreamModel"
            placeholder="上游模型"
            aria-label="上游模型名称"
            @update:model-value="emit('updateMapping', index, 'upstreamModel', $event)"
          />
          <BaseIconButton
            class="col-start-2 row-span-2 row-start-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"
            variant="ghost"
            label="删除映射"
            @click="emit('removeMapping', index)"
          >
            <Trash2 class="size-4 text-cp-error" />
          </BaseIconButton>
        </div>
      </div>
    </div>
  </BaseCard>
</template>
