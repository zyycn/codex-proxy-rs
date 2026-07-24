<script setup lang="ts">
import { GitBranch, Plus, Trash2 } from '@lucide/vue'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const props = withDefaults(defineProps<{
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

const rows = computed(() => props.mappings)
</script>

<template>
  <BaseCard
    :padded="false"
    title="模型映射"
    description="配置请求模型与上游模型的映射关系"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <div class="grid gap-4">
      <div class="flex flex-wrap items-center gap-3">
        <BaseButton variant="default" :disabled="loading" @click="emit('addMapping')">
          <template #icon>
            <Plus class="size-4" />
          </template>
          添加映射
        </BaseButton>
        <span v-if="error" class="text-xs font-[650] text-(--cp-danger-text)">{{ error }}</span>
      </div>

      <div class="flex items-center gap-2 text-[12px] font-[650] text-(--cp-text-secondary)">
        <GitBranch class="size-4 text-(--cp-info)" />
        全局模型映射
      </div>

      <div
        v-if="loading"
        class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-4 text-[13px] font-[650] text-(--cp-text-muted)"
      >
        正在加载模型映射...
      </div>
      <div
        v-else-if="rows.length === 0"
        class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-4 text-[13px] font-[650] text-(--cp-text-muted)"
      >
        暂无模型映射
      </div>
      <div v-else class="grid gap-3">
        <div class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-3 px-1 text-[12px] font-[700] text-(--cp-text-muted)">
          <span>请求模型</span>
          <span />
          <span>上游模型</span>
          <span />
        </div>
        <div
          v-for="(row, index) in rows"
          :key="index"
          class="grid grid-cols-[1fr_auto_1fr_auto] items-center gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) p-3"
        >
          <BaseInput
            :model-value="row.requestedModel"
            placeholder="gpt-5.4"
            aria-label="请求模型"
            @update:model-value="emit('updateMapping', index, 'requestedModel', $event)"
          />
          <span class="text-(--cp-text-muted)">→</span>
          <BaseInput
            :model-value="row.upstreamModel"
            placeholder="gpt-5.5"
            aria-label="上游模型名称"
            @update:model-value="emit('updateMapping', index, 'upstreamModel', $event)"
          />
          <BaseButton
            variant="ghost"
            icon-only
            label="删除映射"
            @click="emit('removeMapping', index)"
          >
            <Trash2 class="size-4 text-(--cp-danger)" />
          </BaseButton>
        </div>
      </div>
    </div>
  </BaseCard>
</template>
