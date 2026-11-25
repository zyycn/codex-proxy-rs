<script setup lang="ts">
import type { RequestProfilePreview } from '@/api'
import { ZSegmented } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import RequestProfilePreviewPanel from './RequestProfilePreviewPanel.vue'

defineProps<{
  allowInherit: boolean
  sideBySide?: boolean
  allowPreserve: boolean
  sourceDisabled: boolean
  sourceLabel?: string
  loadError: string
  showFields: boolean
  showPreview: boolean
  preview?: RequestProfilePreview
  previewing: boolean
  needsInput: boolean
}>()

const { t } = useI18n({ useScope: 'global' })
const source = defineModel<string>({ required: true })
</script>

<template>
  <div class="@container grid min-w-0 gap-4">
    <div v-if="allowInherit" class="flex flex-wrap items-center justify-between gap-3">
      <ZSegmented
        v-model="source"
        :aria-label="sourceLabel ?? t('settings.clientProfile.clientIdentitySource')"
        class="shrink-0"
        :options="[
          ...(allowPreserve ? [{ label: t('accounts.modelAccess.reserve'), value: 'keep' }] : []),
          { label: t('settings.clientProfile.globalConfiguration'), value: 'global' },
          { label: t('settings.clientProfile.customConfiguration'), value: 'independent' },
        ]"
        :disabled="sourceDisabled"
      />
      <slot name="source-extra" />
    </div>
    <p v-if="loadError" role="alert" class="m-0 text-z-sm text-z-error">
      {{ loadError }}
    </p>
    <div
      v-else
      class="grid min-w-0 gap-4"
      :class="sideBySide ? '@5xl:has-[>:nth-child(2)]:grid-cols-2' : undefined"
    >
      <div v-if="showFields" class="grid min-w-0 gap-4">
        <slot />
      </div>
      <!-- 并排时跳过表单标签的高度和间距，预览从第一排输入框开始对齐 -->
      <RequestProfilePreviewPanel
        v-if="showPreview"
        :class="sideBySide && showFields ? '@5xl:mt-[calc(max(1rem,var(--z-font-size))+0.5rem)]' : undefined"
        :preview="preview"
        :previewing="previewing"
        :needs-version-input="needsInput"
      />
    </div>
  </div>
</template>
