<script setup lang="ts">
import { Copy, KeyRound, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'

interface AdminApiKeyStatus {
  exists: boolean
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
    title="管理员 API Key"
    description="用于外部系统集成，具有管理员权限"
  >
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <BaseButton
          variant="secondary"
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
          variant="destructive"
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

    <div class="grid max-w-6xl gap-4">
      <div
        class="flex min-h-16 items-center justify-between gap-4 rounded-cp-control bg-cp-subtle px-4 py-3"
      >
        <div class="flex min-w-0 items-center gap-3">
          <BaseMotionIcon
            aria-hidden="true"
            class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp-control bg-cp-surface text-cp-normal shadow-cp-control"
          >
            <KeyRound class="size-4" />
          </BaseMotionIcon>
          <div class="min-w-0">
            <p class="m-0 text-[13px] leading-[1.15] font-bold text-cp-primary">
              {{ status.exists ? '已启用' : '未生成' }}
            </p>
            <p
              class="mt-1.5 mb-0 truncate text-[12px] leading-[1.15] font-emphasis text-cp-secondary"
            >
              {{
                loading
                  ? '加载中...'
                  : status.exists
                    ? '管理员 API 访问已启用，完整 Key 仅在生成时回显'
                    : '外部系统暂时无法通过 API Key 调用管理接口'
              }}
            </p>
          </div>
        </div>
      </div>

      <div v-if="generatedKey" class="grid gap-2">
        <p class="m-0 text-[13px] leading-[1.15] font-emphasis text-cp-secondary">
          完整 Key 仅显示一次，请立即保存
        </p>
        <div class="flex min-w-0 items-center gap-2">
          <code
            class="min-w-0 flex-1 rounded-cp-control bg-cp-subtle px-3 py-2.5 font-mono text-[12px] leading-normal font-emphasis break-all text-cp-primary"
          >
            {{ generatedKey }}
          </code>
          <BaseIconButton size="md" label="复制" @click="emit('copy')">
            <Copy class="size-4" />
          </BaseIconButton>
        </div>
      </div>
    </div>
  </BaseCard>
</template>
