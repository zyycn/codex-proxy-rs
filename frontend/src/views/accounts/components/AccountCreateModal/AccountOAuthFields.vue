<script setup lang="ts">
import { Copy, KeyRound } from '@lucide/vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { useCopyText } from '@/composables/useCopyText'

defineProps<{
  authUrl: string
  panelTitle: string
  panelDescription: string
  callbackLabel: string
  callbackPlaceholder: string
  loading: boolean
  disabled: boolean
}>()
const emit = defineEmits<{ regenerate: [] }>()
const callback = defineModel<string>({ required: true })
const copyWithToast = useCopyText()
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
        :disabled="disabled"
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
          <div class="rounded-cp bg-[var(--cp-input-bg)] px-3.5 py-3 shadow-cp-tertiary">
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.6] font-emphasis text-cp-text-secondary"
              v-text="authUrl"
            />
          </div>
        </BaseScrollbar>
      </BaseFormItem>
    </BaseForm>

    <BaseForm>
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
