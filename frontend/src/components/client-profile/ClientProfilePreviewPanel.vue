<script setup lang="ts">
import type { ClientProfilePreview } from '@/api/modules/client-profiles'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import { formatDateTime } from '@/utils/date'

defineProps<{
  preview?: Pick<ClientProfilePreview, 'userAgent' | 'versionSource' | 'checkedAt' | 'error'>
  previewing: boolean
  needsVersionInput: boolean
  error: string
}>()
</script>

<template>
  <div class="grid min-h-20 min-w-0 content-center gap-2 rounded-cp bg-cp-fill-quaternary p-4" aria-live="polite" :aria-busy="previewing">
    <p v-if="needsVersionInput" class="m-0 text-cp-sm text-cp-text-tertiary">
      填写版本后预览
    </p>
    <div v-else-if="previewing" class="grid gap-2" role="status" aria-label="正在解析客户端身份">
      <div class="flex h-lh items-center text-cp-sm" aria-hidden="true">
        <BaseSkeleton shape="text" class="w-4/5" />
      </div>
      <div class="flex h-lh items-center text-cp-xs" aria-hidden="true">
        <BaseSkeleton shape="text" class="w-52 max-w-full" />
      </div>
    </div>
    <p v-else-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">
      {{ error }}
    </p>
    <template v-else-if="preview">
      <code class="break-all text-cp-sm text-cp-text">{{ preview.userAgent }}</code>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ preview.versionSource === 'custom' ? '自定义版本' : '自动更新' }}
        <template v-if="preview.versionSource === 'official'">
          · {{ preview.checkedAt ? `检查于 ${formatDateTime(preview.checkedAt)}` : '待检查' }}
        </template>
      </p>
      <p v-if="preview.error && preview.versionSource === 'official'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
        更新失败 · 沿用上次版本
      </p>
    </template>
  </div>
</template>
