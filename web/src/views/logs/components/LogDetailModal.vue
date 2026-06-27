<script setup lang="ts">
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import LogLevelBadge from './LogLevelBadge.vue'
import LogStatusCodeBadge from './LogStatusCodeBadge.vue'

const open = defineModel<boolean>({ default: false })

defineProps<{
  log: any
}>()
</script>

<template>
  <BaseModal
    v-model="open"
    title="日志详情"
    description="查看单条事件的请求、状态和元数据。"
    variant="info"
    width="720px"
  >
    <div v-if="log" class="flex max-h-[min(70vh,760px)] flex-col gap-4 overflow-hidden">
      <div class="grid grid-cols-2 gap-4">
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">时间</label>
          <p class="m-0 text-[13px] text-(--cp-text-primary)">
            {{ log.createdAtDisplay }}
          </p>
        </div>
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">级别</label>
          <LogLevelBadge :level="log.level" />
        </div>
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">请求 ID</label>
          <code class="text-[13px] font-mono text-(--cp-text-primary)">
            {{ log.requestId || '—' }}
          </code>
        </div>
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">状态码</label>
          <LogStatusCodeBadge :status-code="log.statusCode" />
        </div>
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">路由</label>
          <code class="text-[13px] font-mono text-(--cp-text-primary)">
            {{ log.route || '—' }}
          </code>
        </div>
        <div>
          <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">延迟</label>
          <span class="text-[13px] text-(--cp-text-primary)">
            {{ log.latencyMs !== undefined ? `${log.latencyMs}ms` : '—' }}
          </span>
        </div>
      </div>

      <div>
        <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">消息</label>
        <p
          class="m-0 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5 text-[13px] text-(--cp-text-primary)"
        >
          {{ log.message }}
        </p>
      </div>

      <div v-if="log.metadata" class="flex min-h-0 flex-1 flex-col">
        <label class="block text-[11px] font-bold text-(--cp-text-muted) mb-1">元数据</label>
        <BaseScrollbar
          max-height="min(42vh, 420px)"
          view-class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5"
        >
          <pre
            class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.65] text-(--cp-text-primary)"
            >{{ JSON.stringify(log.metadata, null, 2) }}</pre
          >
        </BaseScrollbar>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">关闭</BaseButton>
    </template>
  </BaseModal>
</template>
