<script setup lang="ts">
import type { useAccountOnboarding } from '../composables/useAccountOnboarding'
import type { AccountRow } from '../constants'
import { Copy, KeyRound, Upload } from '@lucide/vue'

import { useClipboard, useFileDialog } from '@vueuse/core'
import { computed, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { toast } from '@/components/base/BaseToast'
import { accountProviderModeOptions } from '../composables/useAccountOnboarding'

type AccountOnboarding = ReturnType<typeof useAccountOnboarding>
type CreateForm = AccountOnboarding['createForm']['value']

const props = withDefaults(
  defineProps<{
    saving?: boolean
    oauthLoading?: boolean
    reauthorizing?: boolean
    account?: AccountRow | null
  }>(),
  {
    saving: false,
    oauthLoading: false,
    reauthorizing: false,
    account: null,
  },
)

const emit = defineEmits<{
  create: []
  generateOauth: []
}>()
const open = defineModel<boolean>({ default: false })
const form = defineModel<CreateForm>('form', { required: true })
const { copy } = useClipboard()

const fileError = ref('')
const { open: openImportFile, onChange: onImportFileChange } = useFileDialog({
  accept: 'application/json,.json',
  multiple: false,
  reset: true,
})

const providerOptions = [
  { label: 'OpenAI', value: 'openai' },
  { label: 'xAI', value: 'xai' },
]
const modeOptions = computed(() => accountProviderModeOptions(form.value.provider))

const provider = computed({
  get: () => form.value.provider,
  set: (value: string) => {
    if (props.reauthorizing)
      return
    form.value = { ...form.value, provider: value }
    fileError.value = ''
  },
})

const mode = computed({
  get: () => form.value.mode,
  set: (value: string) => {
    if (props.reauthorizing && value !== 'oauth')
      return
    form.value = { ...form.value, mode: value }
    fileError.value = ''
  },
})

const tokenText = computed({
  get: () => form.value.tokenText,
  set: (value: string) => {
    form.value = { ...form.value, tokenText: value }
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
const isXai = computed(() => form.value.provider === 'xai')

const accountName = computed(() => {
  return props.account?.email || props.account?.accountId || props.account?.id || '该账号'
})

const modalTitle = computed(() => (props.reauthorizing ? '重新授权账号' : '添加账号'))

const oauthPanelTitle = computed(() => {
  if (props.reauthorizing)
    return accountName.value
  return isXai.value ? '使用 xAI OAuth 完成账号接入' : '使用 OpenAI OAuth 完成账号接入'
})

const oauthPanelDescription = computed(() => {
  if (props.reauthorizing)
    return '生成新的授权链接，完成后粘贴回调地址更新账号凭据'
  return '复制授权链接到浏览器打开，完成后把回调地址粘贴到下方即可导入'
})

const canGenerateOauth = computed(() =>
  !props.saving
  && !props.oauthLoading,
)

const canSubmit = computed(() => {
  if (props.saving || props.oauthLoading)
    return false
  if (mode.value === 'oauth') {
    if (!form.value.oauthFlowId || !oauthAuthUrl.value)
      return false
    return oauthCallback.value.trim().length > 0
  }
  if (mode.value === 'token')
    return tokenText.value.trim().length > 0
  return importText.value.trim().length > 0
})

const description = computed(() => {
  if (props.reauthorizing)
    return '完成授权后粘贴回调地址，系统会更新账号凭据'
  if (isXai.value) {
    return mode.value === 'oauth'
      ? '复制 xAI 授权链接，完成后粘贴回调地址，不使用 xAI API Key'
      : '导入 Grok OAuth 账号 JSON，已存在账号会更新'
  }
  if (mode.value === 'token')
    return '一行一个 Access Token 或 Refresh Token，Access Token 会直接补全账号信息'
  if (mode.value === 'oauth')
    return '复制 OpenAI 授权链接，完成后粘贴回调地址，系统会自动写入或更新账号'
  return '导入 OpenAI OAuth 账号 JSON，系统会按文档结构自动识别'
})

async function loadImportFile(files: FileList | null) {
  fileError.value = ''
  const file = files?.[0]
  if (!file)
    return

  try {
    importText.value = await file.text()
  }
  catch {
    fileError.value = '文件读取失败'
  }
}

onImportFileChange((files) => {
  void loadImportFile(files)
})

async function copyText(value: string, successText: string) {
  if (!value)
    return
  try {
    await copy(value)
    toast.success(successText)
  }
  catch {
    toast.error('复制失败')
  }
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="modalTitle"
    :description="description"
    variant="info"
    width="620px"
    :close-disabled="saving"
  >
    <div class="flex flex-col gap-4">
      <BaseForm v-if="!reauthorizing">
        <div class="grid gap-4">
          <BaseFormItem label="平台" required>
            <BaseSelect
              v-model="provider"
              :options="providerOptions"
              :disabled="saving || oauthLoading"
              aria-label="平台"
            />
          </BaseFormItem>
        </div>
      </BaseForm>

      <BaseSegmented v-if="!reauthorizing" v-model="mode" :options="modeOptions" class="w-full" />

      <BaseForm v-if="mode === 'token'">
        <BaseFormItem label="Token" required>
          <BaseTextarea
            v-model="tokenText"
            aria-label="Token"
            size="md"
            placeholder="eyJ...&#10;rt_..."
            :disabled="saving"
          />
        </BaseFormItem>
      </BaseForm>

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
                {{ oauthPanelTitle }}
              </p>
              <p class="m-0 mt-1 text-[12px] leading-[1.55] font-medium text-(--cp-text-secondary)">
                {{ oauthPanelDescription }}
              </p>
            </div>
          </div>
        </div>

        <div class="flex flex-wrap items-center gap-2">
          <BaseButton
            variant="default"
            :loading="oauthLoading"
            :disabled="!canGenerateOauth"
            @click="emit('generateOauth')"
          >
            {{ reauthorizing ? '重新生成授权链接' : '生成授权链接' }}
          </BaseButton>
        </div>

        <BaseForm v-if="oauthAuthUrl">
          <BaseFormItem label="授权链接">
            <template #extra>
              <BaseButton
                icon-only
                variant="default"
                size="sm"
                title="复制链接"
                label="复制链接"
                :disabled="saving || oauthLoading"
                @click="copyText(oauthAuthUrl, '授权链接已复制')"
              >
                <Copy class="size-3.5" />
              </BaseButton>
            </template>
            <BaseScrollbar
              max-height="92px"
              view-class="rounded-(--cp-input-radius-base) bg-(--cp-input-current-bg,var(--cp-input-context-bg)) px-3.5 py-3 shadow-(--cp-shadow-input)"
            >
              <pre
                class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] font-[650] text-(--cp-text-secondary)"
              >{{ oauthAuthUrl }}</pre>
            </BaseScrollbar>
          </BaseFormItem>
        </BaseForm>

        <BaseForm>
          <BaseFormItem label="回调地址" required>
            <BaseTextarea
              v-model="oauthCallback"
              aria-label="回调地址"
              size="sm"
              :placeholder="isXai ? 'http://127.0.0.1:56121/callback?code=...&state=...' : 'http://localhost:1455/auth/callback?code=...&state=...'"
              :disabled="saving"
            />
          </BaseFormItem>
        </BaseForm>
      </div>

      <BaseForm v-else>
        <BaseFormItem
          label="JSON 内容"
          required
          :error="fileError || undefined"
        >
          <template #extra>
            <BaseButton variant="default" size="sm" :disabled="saving" @click="openImportFile()">
              <template #icon>
                <Upload class="size-3.5" />
              </template>
              上传文件
            </BaseButton>
          </template>
          <BaseTextarea
            v-model="importText"
            aria-label="JSON 内容"
            size="lg"
            placeholder="{&quot;accounts&quot;:[...]}"
            :disabled="saving"
          />
        </BaseFormItem>
      </BaseForm>
    </div>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!canSubmit"
        @click="emit('create')"
      >
        {{ reauthorizing ? '完成重新授权' : mode === 'oauth' ? '完成授权导入' : '导入' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
