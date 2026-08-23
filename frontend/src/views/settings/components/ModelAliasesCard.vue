<script setup lang="ts">
import { GitBranch, Plus, Trash2 } from '@lucide/vue'

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

      <div class="flex items-center gap-2 text-cp-sm font-emphasis text-cp-text-secondary">
        <GitBranch class="size-4 text-cp-primary-text" />
        全局模型映射
      </div>

      <div
        v-if="loading"
        class="rounded-cp bg-cp-fill-quaternary px-4 py-4 text-cp font-emphasis text-cp-text-quaternary"
      >
        正在加载模型映射...
      </div>
      <div
        v-else-if="mappings.length === 0"
        class="rounded-cp bg-cp-fill-quaternary px-4 py-4 text-cp font-emphasis text-cp-text-quaternary"
      >
        暂无模型映射
      </div>
      <div v-else class="grid gap-3">
        <div class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-3 px-1 text-cp-sm font-bold text-cp-text-quaternary">
          <span>请求模型</span>
          <span />
          <span>上游模型</span>
          <span />
        </div>
        <div
          v-for="(row, index) in mappings"
          :key="index"
          class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-3 rounded-cp-card bg-cp-fill-quaternary p-3"
        >
          <BaseInput
            :model-value="row.requestedModel"
            placeholder="gpt-5.4"
            aria-label="请求模型"
            @update:model-value="emit('updateMapping', index, 'requestedModel', $event)"
          />
          <span class="text-cp-text-quaternary">→</span>
          <BaseInput
            :model-value="row.upstreamModel"
            placeholder="gpt-5.5"
            aria-label="上游模型名称"
            @update:model-value="emit('updateMapping', index, 'upstreamModel', $event)"
          />
          <BaseIconButton
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
