<script setup lang="ts">
import type { AccountRow } from '../../constants'
import type { AccountCreateForm } from './model'
import type { AccountCreateProvider } from './presenter'
import { Openai, Xai } from '@boxicons/vue'
import { Copy, KeyRound, LayoutGrid, Upload } from '@lucide/vue'

import { useFileDialog } from '@vueuse/core'
import { computed, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { useCopyText } from '@/composables/useCopyText'
import AccountProviderChooser from './AccountProviderChooser.vue'
import { resolveAccountCreatePresentation } from './presenter'

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
const form = defineModel<AccountCreateForm>('form', { required: true })
const copyWithToast = useCopyText()

const fileError = ref('')
const { open: openImportFile, onChange: onImportFileChange } = useFileDialog({
  accept: 'application/json,.json',
  multiple: false,
  reset: true,
})

const view = computed(() => resolveAccountCreatePresentation({
  form: form.value,
  account: props.account,
  saving: props.saving,
  oauthLoading: props.oauthLoading,
  reauthorizing: props.reauthorizing,
}))

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

function selectProvider(value: AccountCreateProvider) {
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
    :title="view.modal.title"
    :description="view.modal.description"
    :tone="view.modal.tone"
    :size="view.modal.size"
    :dismissible="!saving"
  >
    <template #icon>
      <LayoutGrid v-if="view.isBatch" class="text-cp-text" aria-hidden="true" :width="20" :height="20" />
      <Xai v-else-if="view.isXai" class="text-cp-text" aria-hidden="true" :width="20" :height="20" />
      <Openai v-else class="text-cp-text" aria-hidden="true" :width="20" :height="20" />
    </template>

    <AccountProviderChooser
      v-if="view.choosingProvider"
      :disabled="saving || oauthLoading"
      @select="selectProvider"
    />

    <div v-else class="flex flex-col gap-4">
      <BaseSegmented
        v-if="!reauthorizing && !view.isBatch"
        v-model="mode"
        label="账号添加方式"
        :options="view.modeOptions"
        class="w-full"
      />

      <div v-if="mode === 'oauth'" class="flex flex-col gap-4">
        <div class="rounded-cp bg-cp-fill-quaternary px-4 py-3">
          <div class="flex items-start gap-3">
            <div
              class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-cp bg-cp-bg-container text-cp-primary-text"
            >
              <KeyRound class="size-4" />
            </div>
            <div class="min-w-0 flex-1">
              <p class="m-0 text-cp font-bold text-cp-text">
                {{ view.oauth.panelTitle }}
              </p>
              <p class="m-0 mt-1 text-cp-sm leading-[1.55] font-medium text-cp-text-secondary">
                {{ view.oauth.panelDescription }}
              </p>
            </div>
          </div>
        </div>

        <div class="flex flex-wrap items-center gap-2">
          <BaseButton
            variant="secondary"
            :loading="oauthLoading"
            :disabled="!view.canGenerateOauth"
            @click="emit('generateOauth')"
          >
            {{ reauthorizing ? '重新生成授权链接' : '生成授权链接' }}
          </BaseButton>
        </div>

        <BaseForm v-if="view.oauth.authUrl">
          <BaseFormItem label="授权链接">
            <template #extra>
              <BaseIconButton
                variant="secondary"
                size="sm"
                title="复制链接"
                label="复制链接"
                :disabled="saving || oauthLoading"
                @click="copyText(view.oauth.authUrl, '授权链接已复制')"
              >
                <Copy class="size-3.5" />
              </BaseIconButton>
            </template>
            <BaseScrollbar
              max-height="92px"
            >
              <div class="rounded-cp bg-[var(--cp-input-bg)] px-3.5 py-3 shadow-cp-tertiary">
                <pre
                  class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.6] font-emphasis text-cp-text-secondary"
                  v-text="view.oauth.authUrl"
                />
              </div>
            </BaseScrollbar>
          </BaseFormItem>
        </BaseForm>

        <BaseForm>
          <BaseFormItem :label="view.oauth.callbackLabel" required>
            <BaseTextarea
              v-model="oauthCallback"
              :aria-label="view.oauth.callbackLabel"
              :rows="4"
              :placeholder="view.oauth.callbackPlaceholder"
              :disabled="saving"
            />
          </BaseFormItem>
        </BaseForm>
      </div>

      <BaseForm v-else>
        <BaseFormItem
          :label="view.importInput.label"
          required
          :error="fileError || undefined"
        >
          <template v-if="view.importInput.uploadable" #extra>
            <BaseButton variant="secondary" size="sm" :disabled="saving" @click="openImportFile()">
              <template #icon>
                <Upload class="size-3.5" />
              </template>
              上传文件
            </BaseButton>
          </template>
          <BaseTextarea
            v-model="importText"
            :aria-label="view.importInput.label"
            :rows="9"
            :placeholder="view.importInput.placeholder"
            :disabled="saving"
          />
        </BaseFormItem>
      </BaseForm>
    </div>

    <template v-if="!view.choosingProvider" #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!view.canSubmit"
        @click="emit('create')"
      >
        {{ view.submitLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
