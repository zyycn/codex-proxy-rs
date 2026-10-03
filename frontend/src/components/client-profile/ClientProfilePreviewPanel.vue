<script setup lang="ts">
import type { ClientProfilePreview } from '@/api/modules/client-profiles'
import { BaseSkeleton } from '@codex-proxy/ui'

defineProps<{
  preview?: Pick<ClientProfilePreview, 'userAgent' | 'versionSource' | 'checkedAtDisplay' | 'error'> & { versionLag?: number | null }
  previewing: boolean
  needsVersionInput: boolean
  error: string
}>()
</script>

<template>
  <p v-if="needsVersionInput" role="status" class="m-0 text-cp-sm text-cp-text-tertiary">
    填写版本后预览
  </p>
  <p v-else-if="error && !previewing" role="alert" class="m-0 text-cp-sm text-cp-error">
    {{ error }}
  </p>
  <div
    v-else-if="previewing || preview"
    class="grid min-h-20 min-w-0 content-center gap-2 rounded-cp bg-cp-fill-quaternary p-4"
    aria-live="polite"
    :aria-busy="previewing"
  >
    <template v-if="previewing">
      <div class="flex h-lh items-center text-cp-sm" role="status" aria-label="正在解析客户端身份">
        <BaseSkeleton shape="text" class="w-4/5" aria-hidden="true" />
      </div>
      <div class="flex h-lh items-center text-cp-xs" aria-hidden="true">
        <BaseSkeleton shape="text" class="w-52 max-w-full" />
      </div>
    </template>
    <template v-else-if="preview">
      <code class="break-all text-cp-sm text-cp-text">{{ preview.userAgent }}</code>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ preview.versionSource === 'custom' ? '固定身份' : '自动更新' }}
        <template v-if="preview.versionSource === 'official'">
          <template v-if="preview.versionLag">
            · 滞后 {{ preview.versionLag }} 版
          </template>
          · {{ preview.checkedAtDisplay ? `检查于 ${preview.checkedAtDisplay}` : '待检查' }}
        </template>
      </p>
      <p v-if="preview.error && preview.versionSource !== 'custom'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
        更新失败 · 沿用上次版本
      </p>
    </template>
  </div>
</template>
