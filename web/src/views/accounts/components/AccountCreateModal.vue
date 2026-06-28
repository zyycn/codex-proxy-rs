<script setup lang="ts">
import { useClipboard, useFileDialog } from '@vueuse/core'
import { computed, ref } from 'vue'
import { Copy, KeyRound, Upload } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { toast } from '@/components/base/BaseToast'

const props = withDefaults(
  defineProps<{
    saving?: boolean
    oauthLoading?: boolean
  }>(),
  {
    saving: false,
    oauthLoading: false,
  },
)

const open = defineModel<boolean>({ default: false })
const form = defineModel<any>('form', { required: true })
const { copy } = useClipboard()

const emit = defineEmits<{
  create: []
  generateOauth: []
}>()

const fileError = ref('')
const { open: openImportFile, onChange: onImportFileChange } = useFileDialog({
  accept: 'application/json,.json',
  multiple: false,
  reset: true,
})

const modeOptions = [
  { label: 'OAuth 授权', value: 'oauth' },
  { label: 'RT 导入', value: 'rt' },
  { label: 'CPR', value: 'cpr' },
  { label: 'Sub2API', value: 'sub2api' },
]

const mode = computed({
  get: () => form.value.mode,
  set: (value: string) => {
    form.value = { ...form.value, mode: value }
    fileError.value = ''
  },
})

const refreshToken = computed({
  get: () => form.value.refreshToken,
  set: (value: string) => {
    form.value = { ...form.value, refreshToken: value }
  },
})

const importText = computed({
  get: () => form.value.importText,
  set: (value: string) => {
    form.value = { ...form.value, importText: value }
    fileError.value = ''
  },
})

const oauthCallback = computed({
  get: () => form.value.oauthCallback,
  set: (value: string) => {
    form.value = { ...form.value, oauthCallback: value }
  },
})

const oauthAuthUrl = computed(() => form.value.oauthAuthUrl || '')

const canSubmit = computed(() => {
  if (props.saving || props.oauthLoading) return false
  if (mode.value === 'oauth') {
    return Boolean(
      form.value.oauthSessionId && oauthAuthUrl.value && oauthCallback.value.trim().length > 0,
    )
  }
  if (mode.value === 'rt') return refreshToken.value.trim().length > 0
  return importText.value.trim().length > 0
})

const description = computed(() => {
  if (mode.value === 'rt') {
    return '一行一个 Refresh Token，导入时会自动换取访问令牌并补全账号信息。'
  }
  if (mode.value === 'oauth') {
    return '复制 OpenAI 授权链接，完成后粘贴回调地址，系统会自动写入或更新账号。'
  }
  if (mode.value === 'sub2api') {
    return '导入 Sub2API 导出的账号 JSON，已存在账号会更新。'
  }
  return '导入 Codex Proxy RS 账号 JSON，已存在账号会更新。'
})

async function loadImportFile(files: FileList | null) {
  fileError.value = ''
  const file = files?.[0]
  if (!file) return

  try {
    importText.value = await file.text()
  } catch {
    fileError.value = '文件读取失败'
  }
}

onImportFileChange((files) => {
  void loadImportFile(files)
})

async function copyOAuthAuthUrl() {
  if (!oauthAuthUrl.value) return
  try {
    await copy(oauthAuthUrl.value)
    toast.success('授权链接已复制')
  } catch {
    toast.error('复制失败')
  }
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="添加账号"
    :description="description"
    variant="info"
    width="620px"
    :close-disabled="saving"
  >
    <div class="flex flex-col gap-4">
      <BaseSegmented v-model="mode" :options="modeOptions" class="w-full" />

      <div v-if="mode === 'rt'">
        <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
          Refresh Token <span class="text-(--cp-danger)">*</span>
        </label>
        <textarea
          v-model="refreshToken"
          class="h-40 w-full resize-none rounded-(--cp-input-radius-base) border-0 bg-(--cp-input-current-bg,var(--cp-input-context-bg)) px-3.5 py-3 font-mono text-[12px] leading-[1.55] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow] duration-160 placeholder:text-(--cp-text-muted) hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-(--cp-shadow-input-hover) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus) disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none"
          placeholder="rt_...&#10;rt_..."
          :disabled="saving"
        />
      </div>

      <div v-else-if="mode === 'oauth'" class="flex flex-col gap-4">
        <div class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-3">
          <div class="flex items-start gap-3">
            <div
              class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) text-(--cp-info)"
            >
              <KeyRound class="size-4" />
            </div>
            <div class="min-w-0 flex-1">
              <p class="m-0 text-[13px] font-[720] text-(--cp-text-primary)">
                使用 OpenAI OAuth 完成账号接入
              </p>
              <p class="m-0 mt-1 text-[12px] leading-[1.55] font-medium text-(--cp-text-secondary)">
                复制授权链接到浏览器打开，完成后把回调地址粘贴到下方即可导入。
              </p>
            </div>
          </div>
        </div>

        <div class="flex flex-wrap items-center gap-2">
          <BaseButton
            variant="default"
            :loading="oauthLoading"
            :disabled="saving"
            @click="emit('generateOauth')"
          >
            生成授权链接
          </BaseButton>
        </div>

        <div v-if="oauthAuthUrl" class="flex flex-col gap-2">
          <div class="flex items-center justify-between gap-3">
            <label class="block text-[13px] font-medium text-(--cp-text-secondary)">
              授权链接
            </label>
            <BaseButton
              icon-only
              variant="default"
              size="sm"
              title="复制链接"
              label="复制链接"
              :disabled="saving || oauthLoading"
              @click="copyOAuthAuthUrl"
            >
              <Copy class="size-3.5" />
            </BaseButton>
          </div>
          <BaseScrollbar
            max-height="92px"
            view-class="rounded-(--cp-input-radius-base) bg-(--cp-input-current-bg,var(--cp-input-context-bg)) px-3.5 py-3 shadow-(--cp-shadow-input)"
          >
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] font-[650] text-(--cp-text-secondary)"
              >{{ oauthAuthUrl }}</pre
            >
          </BaseScrollbar>
        </div>

        <div class="flex flex-col gap-2">
          <label class="block text-[13px] font-medium text-(--cp-text-secondary)">
            回调地址 <span class="text-(--cp-danger)">*</span>
          </label>
          <textarea
            v-model="oauthCallback"
            class="h-28 w-full resize-none rounded-(--cp-input-radius-base) border-0 bg-(--cp-input-current-bg,var(--cp-input-context-bg)) px-3.5 py-3 font-mono text-[12px] leading-[1.55] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow] duration-160 placeholder:text-(--cp-text-muted) hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-(--cp-shadow-input-hover) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus) disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none"
            placeholder="http://localhost:1455/auth/callback?code=...&state=..."
            :disabled="saving"
          />
        </div>
      </div>

      <div v-else class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-3">
          <label class="block text-[13px] font-medium text-(--cp-text-secondary)">
            JSON 内容 <span class="text-(--cp-danger)">*</span>
          </label>
          <BaseButton variant="default" size="sm" :disabled="saving" @click="openImportFile()">
            <Upload class="size-3.5" />
            上传文件
          </BaseButton>
        </div>
        <textarea
          v-model="importText"
          class="h-56 w-full resize-none rounded-(--cp-input-radius-base) border-0 bg-(--cp-input-current-bg,var(--cp-input-context-bg)) px-3.5 py-3 font-mono text-[12px] leading-[1.55] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow] duration-160 placeholder:text-(--cp-text-muted) hover:bg-(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover)) hover:shadow-(--cp-shadow-input-hover) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus) disabled:cursor-not-allowed disabled:bg-(--cp-disabled-bg) disabled:text-(--cp-disabled-text) disabled:shadow-none"
          placeholder='{"accounts":[...]}'
          :disabled="saving"
        />
        <p v-if="fileError" class="m-0 text-[12px] font-[650] text-(--cp-danger-text)">
          {{ fileError }}
        </p>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">取消</BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!canSubmit"
        @click="emit('create')"
      >
        {{ mode === 'oauth' ? '完成授权导入' : '导入' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
