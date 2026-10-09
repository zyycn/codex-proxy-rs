<script setup lang="ts">
import { BaseCard, BaseForm, BaseFormItem, BaseIconButton, BaseInput } from '@codex-proxy/ui'

import { CircleHelp, MonitorUp, TerminalSquare } from '@lucide/vue'

defineOptions({ name: 'ClientVersionPolicyCard' })

withDefaults(
  defineProps<{
    loading?: boolean
    desktopError?: string
    cliError?: string
  }>(),
  {
    loading: false,
    desktopError: '',
    cliError: '',
  },
)

const emit = defineEmits<{
  help: []
}>()

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })
</script>

<template>
  <BaseCard>
    <template #title>
      <span class="inline-flex items-center gap-1.5">
        <span>客户端版本限制</span>
        <BaseIconButton
          label="查看安装与升级说明"
          :title="undefined"
          class="size-6! hover:bg-transparent! active:bg-transparent!"
          @click="emit('help')"
        >
          <CircleHelp class="size-3.5" aria-hidden="true" />
        </BaseIconButton>
      </span>
    </template>

    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="Codex Desktop 最低版本"
        description="只检查桌面端应用版本，例如 26.825.51511"
        :error="desktopError"
      >
        <BaseInput
          v-model="minCodexDesktopVersion"
          aria-label="Codex Desktop 最低版本"
          autocomplete="off"
          spellcheck="false"
          placeholder="不限制"
          :disabled="loading"
        >
          <template #prefix>
            <MonitorUp class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="Codex CLI 最低版本"
        description="只检查独立终端版本，例如 0.152.0"
        :error="cliError"
      >
        <BaseInput
          v-model="minCodexCliVersion"
          aria-label="Codex CLI 最低版本"
          autocomplete="off"
          spellcheck="false"
          placeholder="不限制"
          :disabled="loading"
        >
          <template #prefix>
            <TerminalSquare class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
