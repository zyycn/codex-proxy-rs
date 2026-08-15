<script setup lang="ts">
import type { useAccountOnboarding } from '../composables/useAccountOnboarding'
import type { AccountRow } from '../constants'
import { Openai, Xai } from '@boxicons/vue'
import { Copy, KeyRound, LayoutGrid, Upload } from '@lucide/vue'

import { useFileDialog } from '@vueuse/core'
import { computed, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { useCopyText } from '@/composables/useCopyText'
import { accountProviderModeOptions } from '../composables/useAccountOnboarding'
import AccountProviderChooser from './AccountProviderChooser.vue'

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
const copyWithToast = useCopyText()

const fileError = ref('')
const { open: openImportFile, onChange: onImportFileChange } = useFileDialog({
  accept: 'application/json,.json',
  multiple: false,
  reset: true,
})

const modeOptions = computed(() => accountProviderModeOptions(form.value.provider))
const isProviderSelected = computed(() => ['openai', 'xai', 'batch'].includes(form.value.provider))
const isChoosingProvider = computed(() => !props.reauthorizing && !isProviderSelected.value)

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
const isBatch = computed(() => form.value.provider === 'batch')
const importFileLabel = computed(() => {
  if (isBatch.value)
    return '批量账号文件'
  return mode.value === 'agent_identity' ? 'Agent 身份文件' : '账号文件'
})
const importFilePlaceholder = computed(() => mode.value === 'agent_identity'
  ? '粘贴 Agent 身份文件内容'
  : isBatch.value
    ? '粘贴 CPR 多平台导出文件内容'
    : '粘贴 CPR、Sub2API 或 CPA 账号文件内容')

const accountName = computed(() => {
  return props.account?.email || props.account?.accountId || props.account?.id || '该账号'
})

const modalTitle = computed(() => {
  if (props.reauthorizing)
    return '重新授权账号'
  return isChoosingProvider.value ? '选择账号平台' : '导入账号'
})

const oauthPanelTitle = computed(() => {
  if (props.reauthorizing)
    return accountName.value
  return isXai.value ? 'xAI OAuth 授权' : 'OpenAI OAuth 授权'
})

const oauthPanelDescription = computed(() => {
  return isXai.value
    ? '生成并打开授权链接 → 完成浏览器授权 → 粘贴回调地址、查询字符串或授权码'
    : '生成并打开授权链接 → 完成浏览器授权 → 粘贴回调地址'
})

const canGenerateOauth = computed(() =>
  isProviderSelected.value
  && !props.saving
  && !props.oauthLoading,
)

const canSubmit = computed(() => {
  if (!isProviderSelected.value || props.saving || props.oauthLoading)
    return false
  if (mode.value === 'oauth') {
    if (!form.value.oauthFlowId || !oauthAuthUrl.value)
      return false
    return oauthCallback.value.trim().length > 0
  }
  return importText.value.trim().length > 0
})

const description = computed<string | undefined>(() => {
  if (isChoosingProvider.value)
    return undefined
  if (props.reauthorizing) {
    return isXai.value
      ? '完成新的 xAI 授权并替换此账号凭据'
      : '完成新的 OpenAI 授权并替换此账号凭据'
  }
  if (isBatch.value)
    return '粘贴或上传 CPR 账号包，一次导入多个平台账号'
  if (isXai.value) {
    return mode.value === 'oauth'
      ? '通过浏览器授权导入 xAI 账号'
      : '粘贴或上传 xAI 账号文件，匹配已有账号时更新凭据'
  }
  if (mode.value === 'oauth')
    return '通过浏览器授权导入 OpenAI 账号'
  if (mode.value === 'agent_identity')
    return '粘贴或上传 Agent 身份文件，匹配已有账号时更新凭据'
  return '粘贴或上传 CPR、Sub2API 或 CPA 账号文件，匹配已有账号时更新凭据'
})

function selectProvider(value: 'openai' | 'xai' | 'batch') {
  provider.value = value
}

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
  await copyWithToast(value, { successText })
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="modalTitle"
    :description="description"
    :tone="isChoosingProvider ? 'neutral' : 'info'"
    :size="isChoosingProvider ? 'sm' : 'md'"
    :dismissible="!saving"
  >
    <template #icon>
      <LayoutGrid v-if="isBatch" class="text-cp-primary" aria-hidden="true" :width="20" :height="20" />
      <Xai v-else-if="isXai" class="text-cp-primary" aria-hidden="true" :width="20" :height="20" />
      <Openai v-else class="text-cp-primary" aria-hidden="true" :width="20" :height="20" />
    </template>

    <AccountProviderChooser
      v-if="isChoosingProvider"
      :disabled="saving || oauthLoading"
      @select="selectProvider"
    />

    <div v-else class="flex flex-col gap-4">
      <BaseSegmented
        v-if="!reauthorizing && !isBatch"
        v-model="mode"
        label="账号添加方式"
        :options="modeOptions"
        class="w-full"
      />

      <div v-if="mode === 'oauth'" class="flex flex-col gap-4">
        <div class="rounded-cp-control bg-cp-subtle px-4 py-3">
          <div class="flex items-start gap-3">
            <div
              class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-cp-control bg-cp-surface text-cp-info"
            >
              <KeyRound class="size-4" />
            </div>
            <div class="min-w-0 flex-1">
              <p class="m-0 text-[13px] font-bold text-cp-primary">
                {{ oauthPanelTitle }}
              </p>
              <p class="m-0 mt-1 text-[12px] leading-[1.55] font-medium text-cp-secondary">
                {{ oauthPanelDescription }}
              </p>
            </div>
          </div>
        </div>

        <div class="flex flex-wrap items-center gap-2">
          <BaseButton
            variant="secondary"
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
              <BaseIconButton
                variant="secondary"
                size="sm"
                title="复制链接"
                label="复制链接"
                :disabled="saving || oauthLoading"
                @click="copyText(oauthAuthUrl, '授权链接已复制')"
              >
                <Copy class="size-3.5" />
              </BaseIconButton>
            </template>
            <BaseScrollbar
              max-height="92px"
            >
              <div class="rounded-cp-control bg-[var(--cp-input-current-bg,var(--cp-input-context-bg))] px-3.5 py-3 shadow-cp-input">
                <pre
                  class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] font-emphasis text-cp-secondary"
                  v-text="oauthAuthUrl"
                />
              </div>
            </BaseScrollbar>
          </BaseFormItem>
        </BaseForm>

        <BaseForm>
          <BaseFormItem :label="isXai ? '回调地址或授权码' : '回调地址'" required>
            <BaseTextarea
              v-model="oauthCallback"
              :aria-label="isXai ? '回调地址或授权码' : '回调地址'"
              :rows="4"
              :placeholder="isXai ? '回调地址、?code=...&state=... 或授权码' : 'http://localhost:1455/auth/callback?code=...&state=...'"
              :disabled="saving"
            />
          </BaseFormItem>
        </BaseForm>
      </div>

      <BaseForm v-else>
        <BaseFormItem
          :label="importFileLabel"
          required
          :error="fileError || undefined"
        >
          <template #extra>
            <BaseButton variant="secondary" size="sm" :disabled="saving" @click="openImportFile()">
              <template #icon>
                <Upload class="size-3.5" />
              </template>
              上传文件
            </BaseButton>
          </template>
          <BaseTextarea
            v-model="importText"
            :aria-label="importFileLabel"
            :rows="9"
            :placeholder="importFilePlaceholder"
            :disabled="saving"
          />
        </BaseFormItem>
      </BaseForm>
    </div>

    <template v-if="!isChoosingProvider" #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!canSubmit"
        @click="emit('create')"
      >
        {{ reauthorizing ? '完成重新授权' : mode === 'oauth' ? '完成授权导入' : isBatch ? '批量导入' : '导入' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
