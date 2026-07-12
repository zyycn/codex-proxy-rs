<script setup lang="ts">
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountStatusBadge from './AccountStatusBadge.vue'

defineProps<{
  account: any
  status: string
  model: string
  logs: any[]
  error: string
  startedAt: string
  finishedAt: string
  durationMs: number | null
  loadingModels: boolean
  modelOptions: any[]
  statusView: any
}>()

const open = defineModel<boolean>({ default: false })
const selectedModel = defineModel<string>('selectedModel', { required: true })

const emit = defineEmits<{
  test: []
}>()

function connectionLogClass(tone: string) {
  if (tone === 'success') return 'text-(--cp-success-text)'
  if (tone === 'danger') return 'text-(--cp-danger-text)'
  if (tone === 'info') return 'text-(--cp-info-text)'
  return 'text-(--cp-text-secondary)'
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="测试连接"
    description="验证账号令牌、ChatGPT 账号 ID 与 Codex 模型端点是否可用"
    variant="info"
    width="720px"
  >
    <div v-if="account" class="flex flex-col gap-4">
      <section
        class="flex items-center justify-between gap-4 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3"
      >
        <AccountIdentityCell :account="account" size="lg" show-plan />
        <AccountStatusBadge :status="account.status" variant="pill" />
      </section>

      <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3">
        <div class="grid gap-2">
          <label class="text-[12px] font-[760] text-(--cp-text-muted)">测试模型</label>
          <BaseSelect
            v-model="selectedModel"
            :options="modelOptions"
            :disabled="status === 'running' || loadingModels"
            :placeholder="loadingModels ? '加载模型中...' : '选择上游模型'"
            empty-text="上游没有返回模型"
          />
        </div>
      </section>

      <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) p-4">
        <div class="flex items-start justify-between gap-4">
          <div class="flex min-w-0 items-start gap-3">
            <span
              class="inline-flex size-10 shrink-0 items-center justify-center rounded-lg"
              :class="statusView.badge"
            >
              <component
                :is="statusView.icon"
                class="size-5"
                :class="[statusView.iconClass, status === 'running' ? 'animate-pulse' : '']"
              />
            </span>
            <div class="min-w-0">
              <p class="m-0 text-[16px] font-[780] text-(--cp-text-primary)">
                {{ statusView.label }}
              </p>
              <p
                class="mt-1.5 mb-0 text-[13px] leading-normal font-[650] text-(--cp-text-secondary)"
              >
                {{ statusView.description }}
              </p>
            </div>
          </div>
          <span
            class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-[760]"
            :class="statusView.badge"
          >
            {{ status === 'running' ? '检测中' : statusView.label }}
          </span>
        </div>

        <div class="mt-4 grid gap-3 sm:grid-cols-3">
          <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">开始时间</p>
            <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
              {{ startedAt || '-' }}
            </p>
          </div>
          <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">完成时间</p>
            <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
              {{ finishedAt || '-' }}
            </p>
          </div>
          <div class="rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">响应耗时</p>
            <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-primary)">
              {{ durationMs !== null ? `${durationMs}ms` : '-' }}
            </p>
          </div>
        </div>

        <div class="mt-3 rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
          <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">测试模型</p>
          <p
            class="mt-1.5 mb-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
            :title="model || '-'"
          >
            {{ model || '-' }}
          </p>
        </div>

        <div class="mt-3 rounded-lg bg-(--cp-bg-surface) px-3 py-2.5">
          <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">事件轨迹</p>
          <BaseScrollbar max-height="260px" view-class="pt-2 pr-2">
            <div v-if="logs.length === 0" class="text-[12px] font-[650] text-(--cp-text-muted)">
              -
            </div>
            <div v-else class="flex flex-col gap-1.5">
              <div
                v-for="item in logs"
                :key="item.key"
                class="grid grid-cols-[54px_minmax(0,1fr)] gap-2 text-[12px] leading-[1.45] font-[650]"
              >
                <span class="font-mono text-(--cp-text-muted)">{{ item.time }}</span>
                <div class="min-w-0">
                  <p class="m-0 wrap-break-word" :class="connectionLogClass(item.tone)">
                    {{ item.text }}
                  </p>
                  <div v-if="item.detail" class="mt-2 rounded-lg bg-(--cp-bg-subtle) px-3 py-2">
                    <BaseScrollbar max-height="138px" view-class="pr-2">
                      <pre
                        class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[11px] leading-[1.6] font-[620] text-(--cp-text-primary)"
                        >{{ item.detail }}</pre>
                    </BaseScrollbar>
                  </div>
                </div>
              </div>
            </div>
          </BaseScrollbar>
        </div>

        <div v-if="error" class="mt-3 rounded-lg bg-(--cp-danger-bg) px-3 py-2.5">
          <p class="m-0 text-[11px] font-[760] text-(--cp-danger-text)">错误信息</p>
          <BaseScrollbar max-height="118px" view-class="pt-1.5 pr-2">
            <p
              class="m-0 wrap-break-word text-[12px] leading-[1.55] font-[650] text-(--cp-danger-text)"
            >
              {{ error }}
            </p>
          </BaseScrollbar>
        </div>
      </section>
    </div>

    <template #footer>
      <BaseButton variant="ghost" @click="open = false">关闭</BaseButton>
      <BaseButton
        variant="primary"
        :loading="status === 'running'"
        :disabled="!account || loadingModels || !selectedModel"
        @click="emit('test')"
      >
        {{ logs.length > 0 || error ? '重新测试' : '开始测试' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
