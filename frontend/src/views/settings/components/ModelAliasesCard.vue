<script setup lang="ts">
import { GitBranch, Plus, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'

interface AliasRow {
  alias: string
  target: string
}

defineProps<{
  rows: AliasRow[]
  error: string
  disabled: boolean
}>()

const emit = defineEmits<{
  add: []
  update: [index: number, key: keyof AliasRow, value: string]
  remove: [index: number]
}>()
</script>

<template>
  <BaseCard
    :padded="false"
    title="模型映射"
    description="把客户端可见名称指向真实上游模型"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <div class="grid max-w-6xl gap-3">
      <div
        class="hidden grid-cols-[minmax(0,1fr)_minmax(0,1fr)_2.5rem] gap-2 px-0.75 text-xs leading-none font-bold text-(--cp-text-secondary) sm:grid"
      >
        <span>别名</span>
        <span>目标模型</span>
        <span />
      </div>

      <div
        v-if="rows.length === 0"
        class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-3 text-[13px] font-[650] text-(--cp-text-muted)"
      >
        还没有模型映射
      </div>

      <div
        v-for="(row, index) in rows"
        :key="index"
        class="grid items-center gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_2.5rem]"
      >
        <BaseInput
          :model-value="row.alias"
          placeholder="gpt-5.2"
          @update:model-value="emit('update', index, 'alias', $event)"
        >
          <template #prefix>
            <GitBranch class="size-4" />
          </template>
        </BaseInput>
        <BaseInput
          :model-value="row.target"
          placeholder="gpt-5.5"
          @update:model-value="emit('update', index, 'target', $event)"
        />
        <BaseButton
          variant="ghost"
          size="default"
          icon-only
          label="删除映射"
          :disabled="disabled"
          @click="emit('remove', index)"
        >
          <Trash2 class="size-4" />
        </BaseButton>
      </div>

      <div class="flex flex-wrap items-center gap-3 pt-1">
        <BaseButton variant="default" :disabled="disabled" @click="emit('add')">
          <template #icon>
            <Plus class="size-4" />
          </template>
          添加映射
        </BaseButton>
        <span v-if="error" class="text-xs font-[650] text-(--cp-danger-text)">
          {{ error }}
        </span>
      </div>
    </div>
  </BaseCard>
</template>
