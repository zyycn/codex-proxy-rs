<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { Upload } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

const props = withDefaults(
  defineProps<{
    saving?: boolean
  }>(),
  {
    saving: false,
  },
)

const open = defineModel<boolean>({ default: false })
const form = defineModel<any>('form', { required: true })

const emit = defineEmits<{
  create: []
}>()

const fileInput = useTemplateRef<HTMLInputElement>('fileInput')
const fileError = ref('')

const modeOptions = [
  { label: 'RT 导入', value: 'rt' },
  { label: '原生 JSON', value: 'native' },
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

const canSubmit = computed(() => {
  if (props.saving) return false
  if (mode.value === 'rt') return refreshToken.value.trim().length > 0
  return importText.value.trim().length > 0
})

const description = computed(() => {
  if (mode.value === 'rt') {
    return '一行一个 Refresh Token，导入时会自动换取访问令牌并补全账号信息。'
  }
  if (mode.value === 'sub2api') {
    return '导入 Sub2API 导出的账号 JSON，已存在账号会更新。'
  }
  return '导入 Codex Proxy RS 原生账号 JSON，已存在账号会更新。'
})

async function handleFileChange(event: Event) {
  fileError.value = ''
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  if (!file) return

  try {
    importText.value = await file.text()
  } catch {
    fileError.value = '文件读取失败'
  } finally {
    input.value = ''
  }
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="导入账号"
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

      <div v-else class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-3">
          <label class="block text-[13px] font-medium text-(--cp-text-secondary)">
            JSON 内容 <span class="text-(--cp-danger)">*</span>
          </label>
          <BaseButton variant="default" size="sm" :disabled="saving" @click="fileInput?.click()">
            <Upload class="size-4" />
            上传文件
          </BaseButton>
          <input
            ref="fileInput"
            class="hidden"
            type="file"
            accept="application/json,.json"
            @change="handleFileChange"
          />
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
        导入
      </BaseButton>
    </template>
  </BaseModal>
</template>
