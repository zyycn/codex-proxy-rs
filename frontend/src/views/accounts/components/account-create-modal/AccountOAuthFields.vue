<script setup lang="ts">
import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'
import { BaseButton, BaseForm, BaseFormItem, BaseIconButton, BaseScrollbar, BaseTextarea } from '@codex-proxy/ui'
import { Copy, KeyRound } from '@lucide/vue'
import { computed } from 'vue'
import { useCopyText } from '@/composables/useCopyText'

const props = defineProps<{
  authUrl: string
  panelTitle: string
  panelDescription: string
  callbackLabel: string
  callbackPlaceholder: string
  loading: boolean
  disabled: boolean
  authorization: AccountAuthorizationView
  canStart: boolean
}>()
const emit = defineEmits<{ regenerate: [] }>()
const callback = defineModel<string>({ required: true })
const copyWithToast = useCopyText()
const statusText = computed(() => {
  if (props.authorization.status === 'expired')
    return '授权已过期，可重新开始或检查已完成的授权结果'
  if (props.authorization.status === 'paused')
    return '查询已暂停，可检查授权结果后继续'
  return ''
})
</script>

<template>
  <div class="flex flex-col gap-4">
    <div class="rounded-cp bg-cp-fill-quaternary px-4 py-3">
      <div class="flex items-start gap-3">
        <div
          class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-cp bg-cp-bg-container text-cp-primary-text"
        >
          <KeyRound class="size-4" />
        </div>
        <div class="min-w-0 flex-1">
          <p class="m-0 text-cp font-bold text-cp-text">
            {{ panelTitle }}
          </p>
          <p class="m-0 mt-1 text-cp-sm leading-[1.55] font-medium text-cp-text-secondary">
            {{ panelDescription }}
          </p>
        </div>
      </div>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <BaseButton
        variant="secondary"
        :loading="loading"
        :disabled="disabled || (!authorization.flow && !canStart)"
        @click="emit('regenerate')"
      >
        {{ authUrl ? '重新生成授权链接' : '生成授权链接' }}
      </BaseButton>
    </div>

    <BaseForm v-if="authUrl">
      <BaseFormItem label="授权链接">
        <template #extra>
          <BaseIconButton
            variant="secondary"
            size="sm"
            title="复制链接"
            label="复制链接"
            :disabled="disabled"
            @click="copyWithToast(authUrl, { successText: '授权链接已复制' })"
          >
            <Copy class="size-3.5" />
          </BaseIconButton>
        </template>
        <BaseScrollbar max-height="92px">
          <div class="rounded-cp bg-(--cp-input-bg) px-3.5 py-3 shadow-cp-tertiary">
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.6] font-emphasis text-cp-text-secondary"
              v-text="authUrl"
            />
          </div>
        </BaseScrollbar>
      </BaseFormItem>
    </BaseForm>

    <p v-if="statusText" class="m-0 text-cp-sm text-cp-text-secondary" role="status">
      {{ statusText }}
    </p>
    <p v-if="authorization.error" class="m-0 text-cp-sm text-cp-error" role="alert">
      {{ authorization.error }}
    </p>

    <BaseForm v-if="authorization.flow">
      <BaseFormItem :label="callbackLabel" required>
        <BaseTextarea
          v-model="callback"
          :aria-label="callbackLabel"
          :rows="4"
          :placeholder="callbackPlaceholder"
          :disabled="disabled"
        />
      </BaseFormItem>
    </BaseForm>
  </div>
</template>
