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
  return isXai.value ? '使用 xAI OAuth 完成账号接入' : '使用 OpenAI OAuth 完成账号接入'
})

const oauthPanelDescription = computed(() => {
  if (props.reauthorizing) {
    return isXai.value
      ? '生成新的授权链接，完成后粘贴回调地址、含 code 和 state 的查询字符串或授权码更新账号凭据'
      : '生成新的授权链接，完成后粘贴回调地址更新账号凭据'
  }
  return isXai.value
    ? '复制授权链接到浏览器打开，完成后粘贴回调地址、含 code 和 state 的查询字符串或授权码即可导入'
    : '复制授权链接到浏览器打开，完成后把回调地址粘贴到下方即可导入'
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
      ? '完成授权后粘贴回调地址、含 code 和 state 的查询字符串或授权码，系统会更新账号凭据'
      : '完成授权后粘贴回调地址，系统会更新账号凭据'
  }
  if (isBatch.value)
    return '导入 CPR 多平台账号包，系统会按文档中的平台自动分流'
  if (isXai.value) {
    return mode.value === 'oauth'
      ? '复制 xAI 授权链接，完成后粘贴回调地址、含 code 和 state 的查询字符串或授权码，不使用 xAI API Key'
      : '导入 xAI 账号文件，已存在账号会更新'
  }
  if (mode.value === 'oauth')
    return '复制 OpenAI 授权链接，完成后粘贴回调地址，系统会自动写入或更新账号'
  if (mode.value === 'agent_identity')
    return '导入 Agent 身份文件，系统会按身份信息写入或更新账号'
  return '导入 CPR、Sub2API 或 CPA 账号文件，已存在账号会更新'
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
    :variant="isChoosingProvider ? 'default' : 'info'"
    :width="isChoosingProvider ? '420px' : '620px'"
    :close-disabled="saving"
    :hide-footer="isChoosingProvider"
  >
    <template #icon>
      <LayoutGrid v-if="isBatch" class="text-(--cp-text-primary)" aria-hidden="true" :width="20" :height="20" />
      <Xai v-else-if="isXai" class="text-(--cp-text-primary)" aria-hidden="true" :width="20" :height="20" />
      <Openai v-else class="text-(--cp-text-primary)" aria-hidden="true" :width="20" :height="20" />
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
        :options="modeOptions"
        class="w-full"
      />

      <div v-if="mode === 'oauth'" class="flex flex-col gap-4">
        <div class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-4 py-3">
          <div class="flex items-start gap-3">
            <div
              class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-surface) text-(--cp-info)"
            >
              <KeyRound class="size-4" />
            </div>
            <div class="min-w-0 flex-1">
              <p class="m-0 text-[13px] font-bold text-(--cp-text-primary)">
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
                class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.6] font-emphasis text-(--cp-text-secondary)"
              >{{ oauthAuthUrl }}</pre>
            </BaseScrollbar>
          </BaseFormItem>
        </BaseForm>

        <BaseForm>
          <BaseFormItem :label="isXai ? '回调地址或授权码' : '回调地址'" required>
            <BaseTextarea
              v-model="oauthCallback"
              :aria-label="isXai ? '回调地址或授权码' : '回调地址'"
              size="sm"
              :placeholder="isXai ? '粘贴完整回调地址、?code=...&state=... 或裸授权码' : 'http://localhost:1455/auth/callback?code=...&state=...'"
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
            <BaseButton variant="default" size="sm" :disabled="saving" @click="openImportFile()">
              <template #icon>
                <Upload class="size-3.5" />
              </template>
              上传文件
            </BaseButton>
          </template>
          <BaseTextarea
            v-model="importText"
            :aria-label="importFileLabel"
            size="lg"
            :placeholder="importFilePlaceholder"
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
        v-if="!isChoosingProvider"
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
