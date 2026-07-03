<script setup lang="ts">
import { Copy, KeyRound, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'

interface AdminApiKeyStatus {
  exists: boolean
  maskedKey: string | null
}

defineProps<{
  status: AdminApiKeyStatus
  loading: boolean
  regenerating: boolean
  deleting: boolean
  generatedKey: string
}>()

const emit = defineEmits<{
  regenerate: []
  requestDelete: []
  copy: []
}>()
</script>

<template>
  <BaseCard
    :padded="false"
    title="管理员 API Key"
    description="用于外部系统集成，具有管理员权限。"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <BaseButton
          variant="default"
          :loading="regenerating"
          :disabled="loading || deleting"
          @click="emit('regenerate')"
        >
          <template #icon>
            <KeyRound class="size-4" />
          </template>
          {{ status.exists ? '重新生成' : '生成' }}
        </BaseButton>
        <BaseButton
          variant="danger"
          :disabled="loading || regenerating || !status.exists"
          @click="emit('requestDelete')"
        >
          <template #icon>
            <Trash2 class="size-4" />
          </template>
          删除
        </BaseButton>
      </div>
    </template>

    <div class="grid gap-4">
      <div
        class="flex min-h-16 items-center justify-between gap-4 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-3"
      >
        <div class="flex min-w-0 items-center gap-3">
          <span
            class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) text-(--cp-normal) shadow-(--cp-shadow-control)"
          >
            <KeyRound class="size-4" />
          </span>
          <div class="min-w-0">
            <p class="m-0 text-[13px] leading-[1.15] font-[720] text-(--cp-text-primary)">
              {{ status.exists ? '已启用' : '未生成' }}
            </p>
            <p
              class="mt-1.5 mb-0 truncate font-mono text-[12px] leading-[1.15] font-[650] text-(--cp-text-secondary)"
            >
              {{
                loading
                  ? '加载中...'
                  : status.maskedKey || '外部系统暂时无法通过 API Key 调用管理接口'
              }}
            </p>
          </div>
        </div>
      </div>

      <div v-if="generatedKey" class="grid gap-2">
        <p class="m-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">
          完整 Key 仅显示一次，请立即保存。
        </p>
        <div class="flex min-w-0 items-center gap-2">
          <code
            class="min-w-0 flex-1 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 font-mono text-[12px] leading-normal font-[650] break-all text-(--cp-text-primary)"
          >
            {{ generatedKey }}
          </code>
          <BaseButton icon-only size="md" title="复制" @click="emit('copy')">
            <Copy class="size-4" />
          </BaseButton>
        </div>
      </div>
    </div>
  </BaseCard>
</template>
